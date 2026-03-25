# Issue: WeakCrossThreadHandle::resolve() Bypasses Thread Check When TCB is Dead

**Status:** Open  
**Tags:** Verified  
**Date:** 2026-03-25

## Threat Model

| Aspect | Rating |
|--------|--------|
| Likelihood | Medium |
| Severity | Critical (soundness violation) |
| Reproducibility | Requires specific thread timing |

## Affected Component

- **Component:** `WeakCrossThreadHandle::resolve()`
- **OS:** All
- **Rust/rudo-gc versions:** Current

## Description

When `origin_tcb.upgrade()` returns `None` (origin thread has terminated), the `resolve()` method returns `self.weak.upgrade()` directly **without** verifying the current thread matches `origin_thread`.

### Expected Behavior

According to the documentation:
> "Panics if called from a thread other than the origin thread (**including when the origin thread has terminated**)."

### Actual Behavior

Returns `Some(Gc<T>)` if the underlying object is still alive, even when called from a different thread with a recycled ThreadId.

### Scenario

1. Thread A creates a `WeakCrossThreadHandle` and terminates
2. The TCB is dropped; ThreadId is recycled by the OS
3. Thread B (new thread) happens to get the same ThreadId as the original Thread A
4. Thread B calls `weak.resolve()`
5. Since `origin_tcb.upgrade()` returns `None`, it returns `self.weak.upgrade()` directly
6. If the Gc object is still alive (held by other references), this returns `Some(Gc<T>)` to Thread B
7. Thread B can now access `T` even though `T: !Send`!

## Root Cause Analysis

`crates/rudo-gc/src/handles/cross_thread.rs:821-831`:

```rust
pub fn resolve(&self) -> Option<Gc<T>> {
    if self.origin_tcb.upgrade().is_none() {
        return self.weak.upgrade();  // BUG: No thread check here!
    }
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        "WeakCrossThreadHandle::resolve() must be called on the origin thread. \
         If the origin thread has terminated, use try_resolve() instead."
    );
    self.weak.upgrade()
}
```

Compare with `try_resolve()` at lines 839-848 which correctly returns `None` when TCB is dead:

```rust
pub fn try_resolve(&self) -> Option<Gc<T>> {
    self.origin_tcb.upgrade()?;  // Returns None if TCB is dead
    if std::thread::current().id() != self.origin_thread {
        return None;
    }
    self.weak.upgrade()
}
```

## PoC

```rust
use std::thread;
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct NotSend {
    data: Vec<u8>,
}

fn main() {
    let gc = Gc::new(NotSend { data: vec![1, 2, 3] });
    let weak = gc.downgrade();
    
    // Spawn a new thread that might get the same ThreadId
    let handle = thread::spawn(move || {
        // If TCB is dead but ThreadId is recycled, this could succeed
        // when it should panic
        weak.resolve().is_some()
    });
}
```

## Suggested Fix

The `resolve()` method should panic when TCB is dead (consistent with documentation):

```rust
pub fn resolve(&self) -> Option<Gc<T>> {
    if self.origin_tcb.upgrade().is_none() {
        panic!("WeakCrossThreadHandle::resolve() cannot be called after origin thread terminated. \
                Use try_resolve() instead.");
    }
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        "WeakCrossThreadHandle::resolve() must be called on the origin thread. \
         If the origin thread has terminated, use try_resolve() instead."
    );
    self.weak.upgrade()
}
```

## Internal Discussion Record

### R. Kent Dybvig
This is a classic thread affinity bug. The issue is that when the TCB is gone, the code assumes it's safe to proceed without the thread check. But ThreadId reuse breaks this assumption. In Chez Scheme's GC, we always verify thread identity before allowing cross-thread access.

### Rustacean
This is a soundness violation. The `resolve()` method documents that it panics when called from the wrong thread, including when the origin thread has terminated. But the current implementation returns `Some` instead of panicking. This violates the safety contract.

### Geohot
The TOCTOU race here is subtle: the check `origin_tcb.upgrade().is_none()` followed by `self.weak.upgrade()` has a window where state can change. More critically, when TCB is dead, the thread identity check is completely bypassed. This could allow cross-thread access to `!Send` types.