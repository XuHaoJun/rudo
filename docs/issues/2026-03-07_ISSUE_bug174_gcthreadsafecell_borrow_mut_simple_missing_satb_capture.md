# Issue: bug174

**Status**: Verified

**Tags**: Verified

## Threat Model

| Aspect | Assessment |
|--------|------------|
| Likelihood | Low |
| Severity | High |
| Reproducibility | Medium |

## Affected Component

- **Component**: `GcThreadSafeCell::borrow_mut_simple`
- **OS**: All
- **Rust/rudo-gc versions**: Latest

## Description

### Expected Behavior

`GcThreadSafeCell::borrow_mut_simple()` should either:
1. Require `T: GcCapture` and capture old GC pointer values for SATB barrier when incremental marking is active, OR
2. Have runtime checks to prevent misuse with types containing GC pointers

### Actual Behavior

The function requires only `T: Trace` (not `GcCapture`) and does NOT capture old GC pointer values for the SATB barrier when incremental marking is active. It only triggers the page-level write barrier (marks page dirty, adds to remembered buffer) but does NOT record old pointer values in the SATB buffer.

This can lead to memory corruption when:
1. A type contains GC pointers
2. `borrow_mut_simple()` is used instead of `borrow_mut()`
3. Incremental marking is active
4. The old GC pointer values are not recorded in SATB buffer
5. Objects reachable from those old pointers may be prematurely collected

## Root Cause Analysis

In `crates/rudo-gc/src/cell.rs`, the `borrow_mut_simple()` method (lines 1103-1112):

```rust
pub fn borrow_mut_simple(&self) -> parking_lot::MutexGuard<'_, T>
where
    T: Trace,
{
    // Cache barrier states once to avoid TOCTOU (bug116, bug153, bug173)
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);
    self.inner.lock()
}
```

Compare with `borrow_mut()` (lines 1041-1087) which DOES capture old values for SATB:

```rust
if incremental_active {
    let value = &*guard;
    let mut gc_ptrs = Vec::with_capacity(32);
    value.capture_gc_ptrs_into(&mut gc_ptrs);
    if !gc_ptrs.is_empty() {
        // ... record old values in SATB buffer
    }
}
```

The documentation states "It's suitable for types that don't contain any `Gc<T>` pointers" but there's no enforcement - a user could mistakenly use this method with types containing GC pointers, leading to memory corruption.

## Suggested Fix

Option 1: Make `borrow_mut_simple()` require `GcCapture` and capture old values for SATB (like `borrow_mut()` does).

Option 2: Add a runtime assertion that the type doesn't contain GC pointers (less ideal but quick fix).

Option 3: Mark the function as `unsafe` with documentation explaining the requirements.

## Internal Discussion Record

### R. Kent Dybvig
The SATB (Snapshot-At-The-Beginning) barrier requires capturing OLD pointer values BEFORE they are overwritten. The current implementation only marks the page dirty but doesn't capture the actual old values. This is a correctness issue for incremental marking.

### Rustacean
The lack of type enforcement means a user can mistakenly use `borrow_mut_simple()` with types containing GC pointers. The function should either enforce correct usage at compile time or be marked unsafe.

### Geohot
This is a subtle bug that would be hard to debug - it only manifests during incremental marking with specific mutation patterns. The lack of compile-time enforcement makes it a footgun.

---

## Verification (2026-03-07)

Verified by code inspection:
- `borrow_mut()` (cell.rs:1041-1087) captures old values for SATB when `incremental_active` is true (lines 1052-1078)
- `borrow_mut_simple()` (cell.rs:1103-1112) does NOT capture old values - it only triggers `trigger_write_barrier_with_incremental()` but skips the SATB capture logic
- The bug is confirmed: when incremental marking is active and user uses `borrow_mut_simple()` with types containing GC pointers, old pointer values are not recorded in SATB buffer, leading to potential premature collection of objects reachable only from those pointers
