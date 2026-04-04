# [Bug]: AsyncHandle::get reference count leak when object becomes dead/dropping after try_inc_ref_if_nonzero

**Status:** Fixed
**Tags:** Verified

## Threat Model Assessment

| Metric | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | Medium | Race window between try_inc_ref_if_nonzero and dec_ref |
| **Severity** | Medium | Reference count leak, eventually causes premature collection |
| **Reproducibility** | Low | Requires precise timing of concurrent drop |

---

## Affected Component & Environment
- **Component:** `AsyncHandle::get()` (handles/async.rs:630-691) and `AsyncHandle::get_unchecked()` (handles/async.rs:760-795)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior

When `AsyncHandle::get()` increments the reference count via `try_inc_ref_if_nonzero()` and the object subsequently becomes dead or dropping before the final reference release, the increment should be properly rolled back to avoid a reference count leak.

### Actual Behavior

In `AsyncHandle::get()` (handles/async.rs:630-691):

1. Line 648: `try_inc_ref_if_nonzero()` succeeds (ref_count was > 0)
2. Between lines 648-677: Another thread marks the object as dead/dropping
3. Line 677: `dec_ref()` is called, but sees dead_flag is set or dropping_state != 0, returns early WITHOUT decrementing
4. Reference count is now permanently inflated (leak)

The check at lines 666-674 panics before reading the value, so no UAF occurs, but the reference count leak is still a bug.

### Contrast with Handle::get() (bug491)

`Handle::get()` (handles/mod.rs) was just fixed in bug491 to use `undo_inc_ref` instead of `dec_ref`:

```rust
// Line 340 in Handle::get() after bug491 fix:
GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
```

`AsyncHandle::get()` and `AsyncHandle::get_unchecked()` have the SAME bug pattern but were not fixed.

---

## Root Cause Analysis

In `ptr.rs`, `dec_ref()` has early returns:

```rust
// ptr.rs lines 172-183:
pub fn dec_ref(self_ptr: *mut Self) -> bool {
    let this = unsafe { &*self_ptr };
    loop {
        let dead_flag = this.weak_count_raw() & GcBox::<()>::DEAD_FLAG;
        if dead_flag != 0 {
            // Already marked as dead - return false WITHOUT decrementing
            return false;
        }
        if this.is_under_construction() {
            // Object is under construction - return false WITHOUT decrementing
            return false;
        }
        ...
    }
}
```

When `dec_ref()` returns early (without decrementing), the reference count we incremented via `try_inc_ref_if_nonzero()` is never decremented.

---

## Suggested Fix

In `AsyncHandle::get()`, change line 677 from:
```rust
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
```

To:
```rust
GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
```

Similarly for `AsyncHandle::get_unchecked()` at line 781.

---

## Related Bugs

- **bug491**: Handle::get reference count leak (same issue, just fixed)
- **bug474**: GcHandle::try_resolve_impl dec_ref ref_count leak (similar issue, fixed with undo_inc_ref)
- **bug453**: AsyncHandle::get gen mismatch ref_count leak (different issue)

---

## Internal Discussion Record

**R. Kent Dybvig (GC 架構觀點):**
The reference count leak in `AsyncHandle::get()` is similar to bug491 in `Handle::get()`. The pattern is consistent: when using `try_inc_ref_if_nonzero()` followed by a release operation, `dec_ref()` can return early without decrementing if the object becomes dead between the check and the release, causing a leak. The `undo_inc_ref()` function is specifically designed to handle this rollback scenario.

**Rustacean (Soundness 觀點):**
This is a memory leak bug, not a memory safety issue per se. The panic at lines 666-674 prevents UAF by panicking before reading the value when dead/dropping is detected. However, the reference count leak can cause objects to be collected later than expected or never, leading to memory bloat.

**Geohot (Exploit 觀點):**
The TOCTOU race between `try_inc_ref_if_nonzero()` and `dec_ref()` is subtle. An attacker could potentially exploit the timing to cause reference count inflation, though the impact is primarily denial-of-service through memory exhaustion rather than arbitrary code execution.