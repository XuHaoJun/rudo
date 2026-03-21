# [Bug]: GcHandle::drop dec_ref Operating on Potentially Reused Slot

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires specific timing of orphan root removal and lazy sweep |
| **Severity (嚴重程度)** | Critical | dec_ref on wrong object can cause premature object drop |
| **Reproducibility (復現難度)** | Very High | Requires precise thread interleaving with lazy sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::drop` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::drop()` should verify the slot is still allocated before calling `dec_ref()`. If the slot was swept and reused, `dec_ref()` should not be called on the potentially new object.

### 實際行為 (Actual Behavior)

`GcHandle::drop()` removes the orphan root entry and then calls `dec_ref()` WITHOUT any `is_allocated` check. If the slot was swept and reused between these operations, `dec_ref()` would operate on the wrong object.

**Race scenario:**
1. Origin thread terminates, GcHandle becomes orphan
2. `GcHandle::drop()` is called
3. `remove_orphan_root()` is called - returns `Some` if entry existed
4. **Between removal and dec_ref**: lazy sweep runs and reclaims the slot
5. New object B is allocated in the same slot
6. `dec_ref()` is called on B's GcBox - **wrong object!**
7. If B's ref_count is 1, B is prematurely dropped

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `handles/cross_thread.rs` lines 579-593:

```rust
fn drop(&mut self) {
    if self.handle_id == HandleId::INVALID {
        return;
    }
    if let Some(tcb) = self.origin_tcb.upgrade() {
        let mut roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
    } else {
        let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
    }
    self.handle_id = HandleId::INVALID;
    // BUG: dec_ref called without is_allocated check!
    crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
}
```

The orphan path discards the return value of `remove_orphan_root()` with `let _ = ...`, then proceeds to call `dec_ref()` regardless of whether the entry existed.

The issue is:
1. `remove_orphan_root()` returns `Option<usize>` - `Some` if entry existed, `None` if not
2. The return value is discarded
3. Even if entry didn't exist (returned `None`), `dec_ref` is still called
4. Between the failed removal and `dec_ref`, lazy sweep could reclaim and reuse the slot

Additionally, even when `remove_orphan_root()` returns `Some`, there's a TOCTOU window:
1. Entry is removed from orphan table
2. Before `dec_ref` executes, lazy sweep could run and reuse the slot
3. `dec_ref` operates on the new object in the reused slot

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Requires precise thread interleaving:

```rust
// Conceptual PoC - requires TSan or extreme timing control
// Thread 1: GcHandle::drop on orphan handle
// Thread 2: lazy sweep + allocate B in same slot
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check before calling `dec_ref()`:

```rust
fn drop(&mut self) {
    if self.handle_id == HandleId::INVALID {
        return;
    }
    let slot_was_valid = if let Some(tcb) = self.origin_tcb.upgrade() {
        let mut roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
        true  // TCB path: root existed, slot should be valid
    } else {
        match heap::remove_orphan_root(self.origin_thread, self.handle_id) {
            Some(_) => true,  // Entry existed, slot should be valid
            None => false,     // Entry didn't exist, slot may be invalid
        }
    };
    self.handle_id = HandleId::INVALID;
    
    // Only call dec_ref if slot might still be valid
    if slot_was_valid {
        if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                return;  // Slot was swept, don't call dec_ref
            }
        }
    }
    crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The orphan root entry should prevent the object from being collected while the entry exists. However, the TOCTOU window between removing the entry and calling `dec_ref` creates a vulnerability if lazy sweep runs during this window.

**Rustacean (Soundness 觀點):**
Calling `dec_ref` on a potentially reused slot can cause the new object's ref_count to be decremented, potentially leading to premature drop. This is a memory safety issue.

**Geohot (Exploit 觀點):**
Exploit path: (1) Create handle to A, (2) Origin thread terminates, (3) Handle becomes orphan, (4) Another object B is allocated in swept slot, (5) GcHandle::drop calls dec_ref on B, (6) B is prematurely dropped if ref_count was 1.

---

## Related Issues

- bug347: GcHandle::resolve_impl is_allocated check insufficient (same root cause)
- bug206: Missing post-increment is_allocated checks (related but different functions)

---

## Resolution (2026-03-21)

**Outcome:** Already fixed.

The `is_allocated` check is present in the current `GcHandle::drop` implementation at
`crates/rudo-gc/src/handles/cross_thread.rs:675–682`. Before calling `dec_ref`, the code calls
`ptr_to_object_index` + `ptr_to_page_header` and returns early if `!is_allocated(idx)`. This
matches the suggested fix exactly. Existing tests in `tests/bug4_tcb_leak.rs` pass (3/3).