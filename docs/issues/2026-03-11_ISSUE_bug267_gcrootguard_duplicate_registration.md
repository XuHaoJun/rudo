# Issue: bug267 - GcRootGuard duplicate registration causes premature root unregistration

**Status:** Fixed
**Tags:** Verified

## Summary

When multiple `GcRootGuard` instances are created for the same GC pointer, dropping one guard incorrectly unregisters the root while other guards for the same pointer still exist.

## Threat Model

| Aspect | Assessment |
|--------|------------|
| **Likelihood** | Medium - Can occur when user code creates multiple guards for same Gc |
| **Severity** | High - Use-after-free if object is collected while guard is alive |
| **Reproducibility** | High - Deterministic once pattern is understood |
| **Attack Surface** | Any async code using GcRootGuard with potential duplicate guards |

## Affected Component

- **Component**: `GcRootGuard` in tokio/guard.rs
- **OS**: Platform-independent
- **Rust version**: 1.75+
- **rudo-gc version**: All versions with tokio feature

## Expected vs Actual Behavior

**Expected**: When multiple `GcRootGuard` instances protect the same pointer, the root should only be unregistered when the last guard is dropped.

**Actual**: When the first guard is dropped, it unconditionally unregisters the root, even if other guards still exist for the same pointer. This can cause the GC to collect the protected object while other guards are still alive.

## Root Cause Analysis

The root cause is in `GcRootGuard`:

1. `GcRootGuard::new()` calls `GcRootSet::register()` which uses a simple presence check (not reference counting)
2. `GcRootGuard::drop()` unconditionally calls `GcRootSet::unregister()` without checking if other guards exist for the same pointer

The `GcRootSet` uses a `Vec<usize>` to store pointers and only tracks presence (not count):

```rust
// tokio/root.rs - register()
pub fn register(&self, ptr: usize) {
    let mut roots = self.roots.lock().unwrap();
    if !roots.contains(&ptr) {  // Simple presence check
        roots.push(ptr);
        self.dirty.store(true, Ordering::Release);
    }
}

// tokio/guard.rs - Drop impl
impl Drop for GcRootGuard {
    fn drop(&mut self) {
        let ptr_addr = self.ptr.as_ptr() as usize;
        GcRootSet::global().unregister(ptr_addr);  // Always unregisters!
    }
}
```

## Fix Applied

Changed `GcRootSet` to use `HashMap<usize, usize>` for reference counting:

```rust
// tokio/root.rs - Fixed register()
pub fn register(&self, ptr: usize) {
    let mut roots = self.roots.lock().unwrap();
    let count = roots.entry(ptr).or_insert(0);
    *count += 1;
    self.dirty.store(true, Ordering::Release);
}

// tokio/root.rs - Fixed unregister()
pub fn unregister(&self, ptr: usize) {
    let mut roots = self.roots.lock().unwrap();
    if let Some(count) = roots.get_mut(&ptr) {
        *count -= 1;
        if *count == 0 {
            roots.remove(&ptr);
        }
        self.dirty.store(true, Ordering::Release);
    }
    drop(roots);
}
```

## Verification

- [x] Create test that creates multiple guards for same Gc
- [x] Verify object stays alive while any guard exists
- [x] Verify object can be collected after all guards dropped

## Tags

- Verified

## Status

Fixed
