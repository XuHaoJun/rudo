# [Bug]: GcCell::borrow_mut_gen_only inconsistent with GcThreadSafeCell - always triggers barrier

**Status:** Closed
**Tags:** Verified, Fixed

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Any code path using GcCell::borrow_mut_gen_only when generational barrier is disabled |
| **Severity (嚴重程度)** | Low | Performance issue only; barrier does internal early-return optimization |
| **Reproducibility (重現難度)** | Easy | Code inspection confirms inconsistency |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `GcCell::borrow_mut_gen_only` (cell.rs:256-263)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 問題描述 (Description)

### 預期行為

`GcCell::borrow_mut_gen_only` should check `is_generational_barrier_active()` before triggering the barrier, consistent with `GcThreadSafeCell::borrow_mut_gen_only`.

### 實際行為

**`GcCell::borrow_mut_gen_only` (lines 256-263):**
```rust
pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T> {
    self.validate_thread_affinity("borrow_mut_gen_only");
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", false);  // ALWAYS calls!
    self.inner.borrow_mut()
}
```

**`GcThreadSafeCell::borrow_mut_gen_only` (lines 1226-1232):**
```rust
pub fn borrow_mut_gen_only(&self) -> parking_lot::MutexGuard<'_, T> {
    let incremental_active = false;
    let generational_active = crate::gc::incremental::is_generational_barrier_active();  // CHECKS first
    let guard = self.inner.lock();
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);  // Only if active
    guard
}
```

The non-thread-safe version always calls the barrier function regardless of whether the generational barrier is active.

---

## 根本原因分析 (Root Cause Analysis)

The `GcCell::borrow_mut_gen_only` was not updated to match the pattern used in `GcThreadSafeCell::borrow_mut_gen_only`. It should check `is_generational_barrier_active()` before calling `gc_cell_validate_and_barrier`.

Note: The `gc_cell_validate_and_barrier` function has an internal optimization (line 2942-2943) that returns early if generation is 0 and `has_gen_old` is false. However, this is an internal implementation detail - the API should be consistent.

---

## 建議修復方案 (Suggested Fix)

Add a check for `is_generational_barrier_active()` before calling the barrier:

```rust
pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T> {
    self.validate_thread_affinity("borrow_mut_gen_only");

    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    if generational_active {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", false);
    }

    self.inner.borrow_mut()
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The barrier function has internal optimization but API should still be consistent. The check for `generational_active` before calling the barrier is the documented contract.

**Rustacean (Soundness 觀點):**
This is an API inconsistency - the contract should be that `borrow_mut_gen_only` only triggers barriers when they are active.

**Geohot (Exploit 觀點):**
Low severity - the internal optimization prevents incorrect behavior, but unnecessary work is still done when barrier is disabled.

---

## 相關 Issue

- bug445: GcThreadSafeCell::borrow_mut_gen_only lock order fix
- bug153: GcCell generational barrier not cached

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-03-28

**Fix:** Added check for `is_generational_barrier_active()` before calling `gc_cell_validate_and_barrier` in `GcCell::borrow_mut_gen_only`.

**File Changed:** `crates/rudo-gc/src/cell.rs`

**Changes:**
1. Lines 259-264: Added `let generational_active = crate::gc::incremental::is_generational_barrier_active();`
2. Wrapped `gc_cell_validate_and_barrier` call in `if generational_active` block