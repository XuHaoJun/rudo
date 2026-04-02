# [Bug]: Handle::get reference count leak when object becomes dead/dropping after try_inc_ref_if_nonzero

**Status:** Open
**Tags:** Unverified

## Threat Model Assessment

| Metric | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | Medium | Race window between try_inc_ref_if_nonzero and dec_ref |
| **Severity** | Medium | Reference count leak, eventually causes premature collection |
| **Reproducibility** | Low | Requires precise timing of concurrent drop |

---

## Affected Component & Environment
- **Component:** `Handle::get()` (handles/mod.rs:302-358)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior

When `Handle::get()` increments the reference count via `try_inc_ref_if_nonzero()` and the object subsequently becomes dead or dropping before `dec_ref()` is called, the increment should be properly rolled back to avoid a reference count leak.

### Actual Behavior

In `Handle::get()` (handles/mod.rs:302-358):

1. Line 325: `try_inc_ref_if_nonzero()` succeeds (ref_count was > 0)
2. Between lines 325-333: Another thread marks the object as dead/dropping
3. Line 333: `dec_ref()` is called, but sees dead_flag is set or dropping_state != 0, returns early WITHOUT decrementing
4. Reference count is now permanently inflated (leak)

The check at lines 346-353 panics before reading the value, so no UAF occurs, but the reference count leak is still a bug.

### Contrast with Handle::to_gc()

`Handle::to_gc()` (handles/mod.rs:390-439) correctly handles this case:

```rust
// Lines 427-436 in Handle::to_gc():
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    // Use undo_inc_ref, not dec_ref: dec_ref returns early without
    // decrementing when DEAD_FLAG is set or is_under_construction is true,
    // but we need to actually rollback the try_inc_ref_if_nonzero increment.
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("Handle::to_gc: object became dead/dropping after ref increment");
}
```

This is the CORRECT pattern - using `undo_inc_ref` to rollback the increment.

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

In `Handle::get()`, change line 333 from:
```rust
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
```

To use `undo_inc_ref` similar to `Handle::to_gc()`:

```rust
// Before calling dec_ref, check if we need to rollback
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("Handle::get: object became dead/dropping after inc_ref");
}
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
```

Alternatively, since `dec_ref` is being called after an earlier `try_inc_ref_if_nonzero()`, use `undo_inc_ref` directly instead of `dec_ref` (matching the pattern in `GcHandle::try_resolve_impl` bug474 fix).

---

## Related Bugs

- **bug474**: GcHandle::try_resolve_impl dec_ref ref_count leak (similar issue, fixed with undo_inc_ref)
- **bug454**: Handle::get gen mismatch ref_count leak (different issue)
- **bug455**: Handle::to_gc gen mismatch ref_count leak (already correctly uses undo_inc_ref)

---

## Notes

- The panic at line 352 prevents UAF, so this is not a memory safety issue per se
- The reference count leak can cause objects to be collected later than expected (or never)
- This bug is similar to bug474 which was fixed in `GcHandle::try_resolve_impl`
