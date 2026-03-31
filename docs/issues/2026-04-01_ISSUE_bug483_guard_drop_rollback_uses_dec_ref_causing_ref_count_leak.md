# [Bug]: Guard drop rollback uses dec_ref causing ref_count leak on panic

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires panic after inc_ref but before function returns |
| **Severity (嚴重程度)** | Medium | ref_count leak leads to memory leak |
| **Reproducibility (重現難度)** | Very High | Requires specific panic timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `OrphanInsertGuard::drop` (heap.rs:292), `OrphanRootRemoveGuard::drop` (cross_thread.rs:642), `TcbRootRemoveGuard::drop` (cross_thread.rs:661)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When rolling back after a panic (in Drop implementations), the code should use `undo_inc_ref()` which always decrements ref_count, even if the object is dead or under construction.

### 實際行為 (Actual Behavior)

Three guard types use `dec_ref()` for rollback in their Drop implementations. However, `dec_ref()` returns early without decrementing when `DEAD_FLAG` is set or `is_under_construction()` is true. This causes a ref_count leak.

### 程式碼位置

**OrphanInsertGuard::drop (heap.rs:285-294)**:
```rust
impl Drop for OrphanInsertGuard {
    fn drop(&mut self) {
        if orphaned_cross_thread_roots()
            .lock()
            .remove(&(self.thread_id, self.handle_id))
            .is_some()
        {
            GcBox::dec_ref(self.ptr.as_ptr());  // BUG: Should be undo_inc_ref
        }
    }
}
```

**OrphanRootRemoveGuard::drop (cross_thread.rs:639-644)**:
```rust
impl Drop for OrphanRootRemoveGuard {
    fn drop(&mut self) {
        if heap::remove_orphan_root(self.thread_id, self.handle_id).is_some() {
            GcBox::dec_ref(self.ptr.as_ptr());  // BUG: Should be undo_inc_ref
        }
    }
}
```

**TcbRootRemoveGuard::drop (cross_thread.rs:656-662)**:
```rust
impl Drop for TcbRootRemoveGuard {
    fn drop(&mut self) {
        let mut roots = self.tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
        GcBox::dec_ref(self.ptr.as_ptr());  // BUG: Should be undo_inc_ref
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### dec_ref vs undo_inc_ref 的差異

From `ptr.rs` documentation (lines 219-227):

> "Use this instead of `dec_ref` when rolling back a successful `try_inc_ref_from_zero` or `try_inc_ref_if_nonzero` CAS: `dec_ref` returns early without decrementing when `DEAD_FLAG` is set, leaving `ref_count` incorrectly at 1."

`dec_ref()` behavior:
- Returns early without decrementing if `DEAD_FLAG` is set
- Returns early without decrementing if `is_under_construction()` is true

### 問題流程

1. Function calls `inc_ref` on a GcBox
2. Panic occurs before function returns
3. Guard's `Drop::drop` is called
4. `remove_orphan_root` or similar succeeds (entry exists)
5. `dec_ref()` is called to rollback - but returns early if object is dead/under construction!
6. **Result**: ref_count is one too high → memory leak

### 對比其他函數的正確實現

- `Handle::get` (mod.rs:328-331): Uses `undo_inc_ref` correctly
- `Handle::to_gc` (mod.rs:416-418): Uses `undo_inc_ref` correctly
- `GcHandle::resolve_impl` (cross_thread.rs:282): Uses `undo_inc_ref` (bug478 fix)
- `GcHandle::try_resolve_impl` (cross_thread.rs:422): Uses `undo_inc_ref` (bug478 fix)

---

## 💣 重現步驟 / 概念驗證 (PoC)

理論 PoC（極難穩定重現）:
```rust
// 需要精確控制執行緒 interleaving 和 panic timing
// 1. Thread A: Calls function that creates OrphanInsertGuard and inc_refs
// 2. Thread B: Last strong ref dropped, DEAD_FLAG set on object
// 3. Thread A: Panic occurs before function returns
// 4. OrphanInsertGuard::drop is called
// 5. dec_ref() returns early without decrementing (DEAD_FLAG set)
// 6. Result: ref_count leak
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

Replace `dec_ref` with `undo_inc_ref` in all three guard drop implementations:

```rust
// OrphanInsertGuard::drop line 292:
GcBox::undo_inc_ref(self.ptr.as_ptr());

// OrphanRootRemoveGuard::drop line 642:
GcBox::undo_inc_ref(self.ptr.as_ptr());

// TcbRootRemoveGuard::drop line 661:
GcBox::undo_inc_ref(self.ptr.as_ptr());
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Guard rollback is critical for reference counting correctness. Panic paths must use `undo_inc_ref` to ensure proper rollback regardless of object state.

**Rustacean (Soundness 觀點):**
Memory leak 不是嚴格的 UB，但長期累積可能導致記憶體無法回收。與 bug478 相同模式但不同程式碼路徑。

**Geohot (Exploit 攻擊觀點):**
理論上可被利用造成 memory leak，但需要精確控制 panic timing 和執行緒時序。

---

## 相關 Bug

- bug378: dec_ref used instead of undo_inc_ref for rollback (已修復)
- bug478: GcHandle::resolve_impl and try_resolve_impl same issue (已修復)
- bug483: This issue - Guard drop rollback (current)
