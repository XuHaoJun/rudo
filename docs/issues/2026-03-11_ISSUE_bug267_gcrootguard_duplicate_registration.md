# Issue: bug267 - GcRootGuard duplicate registration causes premature root unregistration

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

## PoC

```rust
use rudo_gc::{Gc, Trace, GcTokioExt};
use std::sync::Arc;
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc = Gc::new(Data { value: 42 });
    
    // Create two guards for the same Gc
    let guard1 = gc.root_guard();
    let guard2 = gc.root_guard();
    
    // Drop the first guard - root is now unregistered
    drop(guard1);
    
    // But guard2 is still alive! Root was incorrectly removed.
    // GC could now collect the object even though guard2 exists.
    
    // Access via guard2 could cause use-after-free
    let _ = *guard2.resolve(); // May point to freed memory!
}
```

## Suggested Fix

Use reference counting in `GcRootSet` to track how many guards protect each pointer:

1. Change `GcRootSet::roots` from `Vec<usize>` to `HashMap<usize, usize>` (ptr -> count)
2. Modify `register()` to increment count instead of just checking presence
3. Modify `unregister()` to decrement count and only remove when count reaches 0

Alternatively, use RAII properly by making `GcRootGuard` hold an `Arc`-like reference to prevent duplicates at the guard level.

## Internal Discussion Record

### R. Kent Dybvig
The issue is fundamentally about reference counting at the root set level. GC systems typically need accurate root counts to determine when objects can be collected. The current implementation treats roots as boolean (present/absent) rather than counted, which breaks down when multiple paths to the same root exist.

### Rustacean
This is a classic reference counting bug. The safety invariant violated is: "As long as any GcRootGuard exists for a pointer, that pointer must remain a GC root." The current code violates this by removing the root on any drop.

### Geohot
The exploit potential here is clear - if an attacker can arrange for guard1 to be dropped before guard2 (e.g., via exception handling or async task cancellation), they could trigger use-after-free by causing the protected object to be collected while another reference to it still exists.

## Verification

- [ ] Create test that creates multiple guards for same Gc
- [ ] Verify object stays alive while any guard exists
- [ ] Verify object can be collected after all guards dropped

## Tags

- Unverified

## Status

Open
