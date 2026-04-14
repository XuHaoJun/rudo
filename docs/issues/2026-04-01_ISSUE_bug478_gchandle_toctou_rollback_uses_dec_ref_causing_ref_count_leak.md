# [Bug]: GcHandle::resolve_impl and try_resolve_impl TOCTOU rollback uses dec_ref causing ref_count leak

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires precise timing: object becomes dead between inc_ref and post-check |
| **Severity (嚴重程度)** | Medium | ref_count leak leads to memory leak |
| **Reproducibility (重現難度)** | Very High | Requires specific thread interleaving |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve_impl()` (cross_thread.rs:282) and `GcHandle::try_resolve_impl()` (cross_thread.rs:422)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When rolling back after detecting an object became dead/dropping/under_construction between the pre-check and `inc_ref()`, the code should use `undo_inc_ref()` which always decrements ref_count.

### 實際行為 (Actual Behavior)

Both `resolve_impl` and `try_resolve_impl` use `dec_ref()` for the TOCTOU rollback case. However, `dec_ref()` returns early without decrementing when `DEAD_FLAG` is set or `is_under_construction()` is true. This causes a ref_count leak.

### 程式碼位置

**resolve_impl (cross_thread.rs:276-284)**:
```rust
// Post-increment safety check (TOCTOU: object may have been dropped between
// pre-check and inc_ref). Same pattern as Weak::upgrade.
if gc_box.dropping_state() != 0
    || gc_box.has_dead_flag()
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(self.ptr.as_ptr());  // BUG: Should be undo_inc_ref
    panic!("GcHandle::resolve: object was dropped after inc_ref (TOCTOU race)");
}
```

**try_resolve_impl (cross_thread.rs:417-424)**:
```rust
// Post-increment safety check (TOCTOU). Same pattern as Weak::try_upgrade.
if gc_box.dropping_state() != 0
    || gc_box.has_dead_flag()
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(self.ptr.as_ptr());  // BUG: Should be undo_inc_ref
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### dec_ref vs undo_inc_ref 的差異

From `ptr.rs` documentation (lines 219-227):

> "Use this instead of `dec_ref` when rolling back a successful `try_inc_ref_from_zero` or `try_inc_ref_if_nonzero` CAS: `dec_ref` returns early without decrementing when `DEAD_FLAG` is set, leaving `ref_count` incorrectly at 1."

`dec_ref()` behavior (ptr.rs:168-182):
```rust
pub fn dec_ref(self_ptr: *mut Self) -> bool {
    let dead_flag = this.weak_count_raw() & GcBox::<()>::DEAD_FLAG;
    if dead_flag != 0 {
        // Return false WITHOUT decrementing!
        return false;
    }
    if this.is_under_construction() {
        // Return false WITHOUT decrementing!
        return false;
    }
    // ... actual decrement logic
}
```

### 問題流程

1. `resolve_impl` / `try_resolve_impl`: Pre-check passes (object alive)
2. `inc_ref()` or `try_inc_ref_if_nonzero()` succeeds
3. **Race window**: Another thread marks object as dead (sets DEAD_FLAG)
4. TOCTOU check at line 278-280 / 418-420: `dropping_state() != 0 || has_dead_flag() || is_under_construction()` is TRUE
5. `dec_ref()` is called to rollback - but returns early without decrementing!
6. **Result**: ref_count is now one too high → memory leak

### 對比其他函數的正確實現

- `Handle::get` (mod.rs:328-331): Uses `undo_inc_ref` correctly
- `Handle::to_gc` (mod.rs:416-418): Uses `undo_inc_ref` correctly
- `GcBoxWeakRef::upgrade` (ptr.rs:723-726): Uses `undo_inc_ref` correctly
- `GcHandle::resolve_impl` generation check (line 272): Uses `undo_inc_ref` correctly
- `GcHandle::try_resolve_impl` generation check (line 413): Uses `undo_inc_ref` correctly

---

## 💣 重現步驟 / 概念驗證 (PoC)

理論 PoC（極難穩定重現）:
```rust
// 需要精確控制執行緒 interleaving
// 1. Thread A: resolve_impl() pre-check passes
// 2. Thread B: Last strong ref dropped, DEAD_FLAG set
// 3. Thread A: inc_ref() succeeds on object with DEAD_FLAG
// 4. Thread A: TOCTOU check sees DEAD_FLAG, calls dec_ref()
// 5. dec_ref() returns early without decrementing
// 6. Result: ref_count leak
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

Replace `dec_ref` with `undo_inc_ref` in both locations:

```rust
// resolve_impl line 282:
GcBox::undo_inc_ref(self.ptr.as_ptr());  // Always decrements, even if DEAD_FLAG set

// try_resolve_impl line 422:
GcBox::undo_inc_ref(self.ptr.as_ptr());  // Always decrements, even if DEAD_FLAG set
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
TOCTOU rollback is critical for reference counting correctness. If dec_ref doesn't actually decrement in the rollback case, ref_count can become inflated, leading to memory leaks.

**Rustacean (Soundness 觀點):**
Memory leak 不是嚴格的 UB，但長期累積可能導致記憶體無法回收。

**Geohot (Exploit 攻擊觀點):**
理論上可被利用造成 memory leak，但需要精確控制執行緒時序。

---

## 相關 Bug

- bug378: dec_ref used instead of undo_inc_ref for rollback (已修復)
- bug454: Handle::get() same issue (已修復)
- bug455: Handle::to_gc() same issue (已修復)
- bug461: GcHandle::resolve_impl generation check (已修復)
- bug474: GcHandle::try_resolve_impl generation/slot swept cases (已修復，但 TOCTOU case 未修復)
