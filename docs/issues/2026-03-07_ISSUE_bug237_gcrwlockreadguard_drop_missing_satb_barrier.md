# [Bug]: GcRwLockReadGuard::drop 缺少 SATB Barrier 捕獲

**Status:** Fixed
**Tags:** Fixed

---

## 📊 Threat Model Assessment

| Aspect | Assessment |
|--------|------------|
| Likelihood | Medium |
| Severity | High |
| Reproducibility | Medium |

---

## 🧩 Affected Component & Environment

- **Component:** `GcRwLockReadGuard::drop()` in `sync.rs:377-381`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 Description

### Expected Behavior

`GcRwLockReadGuard::drop()` should capture and mark GC pointers on drop, similar to `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()`. This is necessary for SATB (Snapshot-At-The-Beginning) barrier to work correctly during incremental marking.

### Actual Behavior

`GcRwLockReadGuard::drop()` only drops the parking_lot read guard without capturing any GC pointers:

```rust
impl<T: ?Sized> Drop for GcRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        // Guard is dropped automatically when it goes out of scope
        // The parking_lot guard will release the read lock
    }
}
```

This is inconsistent with:
- `GcRwLockWriteGuard::drop()` - which captures and marks GC pointers
- `GcMutexGuard::drop()` - which captures and marks GC pointers

---

## 🔬 Root Cause Analysis

When a user acquires a read lock on `GcRwLock<Gc<T>>`, modifies the inner value (e.g., clones the Gc<T>), and then drops the read guard, the SATB barrier should fire to capture the new Gc pointer. However, since `GcRwLockReadGuard::drop()` doesn't capture anything, the new GC pointer is not marked, potentially leading to the young GC object being prematurely collected.

This is particularly dangerous in incremental marking mode where the SATB barrier is critical for correctness.

---

## 💣 Steps to Reproduce / PoC

```rust
// Requires incremental marking to be active:
// 1. Create a GcRwLock<Gc<T>> with some inner value
// 2. Acquire read lock
// 3. Clone the inner Gc<T> to store in another data structure
// 4. Drop the read guard
// 5. Trigger minor GC (collect())
// 6. The cloned Gc<T> might be incorrectly collected because SATB barrier didn't fire
```

---

## 🛠️ Suggested Fix / Remediation

Add SATB barrier capture to `GcRwLockReadGuard::drop()`, similar to the implementation in `GcRwLockWriteGuard::drop()`:

```rust
impl<T: ?Sized> Drop for GcRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        // Capture GC pointers for SATB barrier
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        // Mark captured pointers
        for gc_ptr in ptrs {
            let _ = unsafe { 
                crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8) 
            };
        }
    }
}
```

Note: This requires `T: GcCapture` bound on the struct. Looking at `GcRwLockReadGuard`, it currently has `T: ?Sized` but not `GcCapture`. This needs to be added.

---

## 🗣️ Internal Discussion Record

### R. Kent Dybvig
The SATB barrier is critical for incremental marking correctness. `GcRwLockReadGuard` should capture GC pointers on drop because:
1. Even with a read lock, the user can clone Gc pointers from the protected data
2. These cloned pointers become roots that must be traced by the GC
3. The barrier ensures these new roots are marked in the current GC cycle

### Rustacean
The missing SATB capture in `GcRwLockReadGuard::drop()` could lead to:
1. Young GC objects being prematurely collected
2. Use-after-free if the Gc pointer becomes invalid
3. Inconsistency with `GcRwLockWriteGuard` and `GcMutexGuard` which both have SATB capture

This is a soundness issue because it can lead to memory safety violations.

### Geohot
An attacker who can control GC timing could:
1. Trigger incremental marking
2. Acquire read lock on GcRwLock<Gc<T>>
3. Clone the inner Gc<T>
4. Drop the read guard
5. Trigger minor GC before the cloned Gc<T> is traced
6. The object is incorrectly collected, leading to use-after-free

This is especially problematic in async contexts where GC can yield to mutators between incremental marking steps.
