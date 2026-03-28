# [Bug]: GcThreadSafeCell::borrow_mut_gen_only triggers write barrier before acquiring lock (inconsistent API)

**Status:** Fixed
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | All calls to `borrow_mut_gen_only` trigger this pattern |
| **Severity (嚴重程度)** | Medium | API inconsistency could lead to maintenance issues and potential future bugs |
| **Reproducibility (重現難度)** | Very Easy | Code inspection confirms the inconsistency |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `GcThreadSafeCell::borrow_mut_gen_only` (cell.rs:1226-1231)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcThreadSafeCell::borrow_mut_gen_only` should acquire the mutex lock BEFORE triggering the write barrier, consistent with `borrow_mut` and `borrow_mut_simple`.

### 實際行為 (Actual Behavior)

In `borrow_mut_gen_only` (lines 1226-1231):
```rust
pub fn borrow_mut_gen_only(&self) -> parking_lot::MutexGuard<'_, T> {
    let incremental_active = false;
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);  // BARRIER BEFORE LOCK!
    self.inner.lock()  // LOCK ACQUIRED AFTER!
}
```

**Contrast with `borrow_mut` (lines 1063-1107):**
```rust
let guard = self.inner.lock();  // Lock FIRST
// ...
self.trigger_write_barrier_with_incremental(incremental_active, generational_active);  // Barrier AFTER lock
```

**Contrast with `borrow_mut_simple` (lines 1151-1190):**
```rust
let guard = self.inner.lock();  // Lock FIRST
// ...
self.trigger_write_barrier_with_incremental(incremental_active, generational_active);  // Barrier AFTER lock
```

---

## 根本原因分析 (Root Cause Analysis)

The bug is a simple ordering issue. The consistent pattern in this API is:
1. Acquire the lock
2. Trigger write barrier
3. Perform operation

`borrow_mut_gen_only` violates this pattern by triggering the barrier before acquiring the lock.

---

## 建議修復方案 (Suggested Fix)

Reorder the operations to acquire the lock first:

```rust
pub fn borrow_mut_gen_only(&self) -> parking_lot::MutexGuard<'_, T> {
    let incremental_active = false;
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    let guard = self.inner.lock();  // Acquire lock FIRST
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);  // Barrier AFTER lock
    guard
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The write barrier is designed to operate under the protection of the mutex lock. Triggering it before acquiring the lock creates an inconsistent API that could lead to maintenance issues.

**Rustacean (Soundness 觀點):**
While the barrier may still function correctly in most cases, the inconsistent ordering violates the principle of least surprise and could mask future bugs.

**Geohot (Exploit 觀點):**
The barrier-before-lock pattern could theoretically allow another thread to observe inconsistent state if it races with `borrow_mut_gen_only`.

---

## 相關 Issue

- bug116: trigger_write_barrier TOCTOU
- bug153: GcCell generational barrier not cached

---

## Resolution (2026-03-28)

`GcThreadSafeCell::borrow_mut_gen_only` in `cell.rs` acquires `self.inner.lock()` before `trigger_write_barrier_with_incremental`, matching `borrow_mut` / `borrow_mut_simple`. No further code change required; issue closed as already fixed in tree.