# [Bug]: borrow_mut_gen_only() never captures OLD GC pointers for SATB

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires major GC without incremental marking while generational barrier is active |
| **Severity (嚴重程度)** | `High` | Missing SATB barrier could cause UAF during major GC |
| **Reproducibility (復現難度)** | `High` | Need specific GC timing: major GC without incremental |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut_gen_only()` (cell.rs:272-292)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`borrow_mut_gen_only()` is documented as an "escape hatch for performance-critical code where barrier overhead is measurable." However, it still marks pages dirty when `generational_active` is true (lines 286-289), suggesting it partially handles generational barriers.

The bug: `borrow_mut_gen_only()` **never captures OLD GC pointers for SATB recording**. Compare with `borrow_mut()` (lines 174-192) which captures and records OLD values to preserve the SATB invariant.

### 預期行為 (Expected Behavior)

If `borrow_mut_gen_only()` marks pages dirty when `generational_active` is true, it should also capture OLD GC pointers for SATB. Alternatively, if it's truly a "no barriers" escape hatch, it should NOT mark pages dirty at all.

### 實際行為 (Actual Behavior)

`borrow_mut_gen_only()` calls `gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", false)` when `generational_active` is true (line 278), which marks the page dirty but does NOT record OLD values to SATB.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `borrow_mut_gen_only()`:

```rust
pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T> {
    self.validate_thread_affinity("borrow_mut_gen_only");

    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    if generational_active {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", false);
    }

    // FIX bug630: Always mark page dirty when borrow_mut_gen_only is called.
    // This ensures children in GcCell<Vec<Gc<T>>> are traced during minor GC.
    unsafe {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        crate::heap::mark_page_dirty_for_borrow(ptr);
    }

    self.inner.borrow_mut()
}
```

1. Line 275: Caches `generational_active`
2. Lines 276-279: If generational is active, calls `gc_cell_validate_and_barrier` with `incremental_active = false`
3. Lines 286-289: Marks page dirty

**Missing**: There is NO code to capture OLD GC pointers and record them to SATB.

Compare with `borrow_mut()` (lines 174-192):

```rust
// FIX bug486: Always capture old GC pointers for SATB, regardless of incremental_active.
{
    unsafe {
        let value = &*self.inner.as_ptr();
        let mut gc_ptrs = Vec::with_capacity(32);
        value.capture_gc_ptrs_into(&mut gc_ptrs);
        if !gc_ptrs.is_empty() {
            crate::heap::with_heap(|heap| {
                for gc_ptr in gc_ptrs {
                    if !heap.record_satb_old_value(gc_ptr) {
                        // ...
                    }
                }
            });
        }
    }
}
```

This capture and record logic is entirely absent from `borrow_mut_gen_only()`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable incremental marking with `set_incremental_config()`
2. Create a `GcCell<Gc<T>>` with an old GC pointer
3. Call `borrow_mut_gen_only()` to mutate the inner `Gc<T>`
4. Trigger a major GC without incremental marking (or with incremental disabled)
5. The OLD GC pointer may not be traced, causing UAF

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Either:

**Option A**: Add SATB capture to `borrow_mut_gen_only()`:
```rust
if generational_active || incremental_active {
    // Capture and record OLD GC pointers for SATB
    unsafe {
        let value = &*self.inner.as_ptr();
        let mut gc_ptrs = Vec::with_capacity(32);
        value.capture_gc_ptrs_into(&mut gc_ptrs);
        if !gc_ptrs.is_empty() {
            crate::heap::with_heap(|heap| {
                for gc_ptr in gc_ptrs {
                    let _ = heap.record_satb_old_value(gc_ptr);
                }
            });
        }
    }
}
```

**Option B**: Remove the dirty marking from `borrow_mut_gen_only()` if it's truly a "no barriers" escape hatch. Currently it's inconsistent - marking dirty but not recording SATB.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The function `borrow_mut_gen_only` is marketed as a "no barriers" escape hatch, but it still marks pages dirty. This is inconsistent. If you're marking dirty, you need to record SATB for major GC correctness. The gen_old optimization handles OLD→YOUNG for minor GC, but major GC with incremental disabled would miss SATB.

**Rustacean (Soundness 觀點):**
The inconsistency between `borrow_mut()` and `borrow_mut_gen_only()` suggests the latter is missing SATB recording. If `generational_active` can be true during a major GC cycle (without incremental), not recording OLD values could cause the GC to miss references, leading to UAF.

**Geohot (Exploit 觀點):**
If an attacker can control when major GC runs without incremental, they could trigger `borrow_mut_gen_only` on a `GcCell<Vec<Gc<T>>>` containing OLD→YOUNG references. The missing SATB recording could cause the young object to be prematurely collected, creating a use-after-free that could be exploited.

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-04-14
**Fix:** Added SATB capture to `borrow_mut_gen_only()` in `cell.rs`.

**Code Change:**
- Added `incremental_active` caching alongside `generational_active`
- Added SATB capture block when `generational_active || incremental_active`
- Changed `gc_cell_validate_and_barrier` call to pass `incremental_active` instead of `false`

```rust
let generational_active = crate::gc::incremental::is_generational_barrier_active();
let incremental_active = crate::gc::incremental::is_incremental_marking_active();

// FIX bug635: Capture OLD GC pointers for SATB when any barrier is active.
if generational_active || incremental_active {
    unsafe {
        let value = &*self.inner.as_ptr();
        let mut gc_ptrs = Vec::with_capacity(32);
        value.capture_gc_ptrs_into(&mut gc_ptrs);
        if !gc_ptrs.is_empty() {
            crate::heap::with_heap(|heap| {
                for gc_ptr in gc_ptrs {
                    let _ = heap.record_satb_old_value(gc_ptr);
                }
            });
        }
    }
}

if generational_active {
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", incremental_active);
}
```

**Verification:** `./clippy.sh` passes. Library tests pass.
