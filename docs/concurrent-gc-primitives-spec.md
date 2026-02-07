# Concurrent GC Primitives: GcRwLock and GcMutex

**Status**: Draft
**Date**: 2026-02-08
**Components**: `transducer-gc` (new `sync` module)

## 1. Executive Summary

This specification introduces thread-safe interior mutability primitives (`GcRwLock` and `GcMutex`) to `rudo-gc`, enabling garbage-collected objects to be safely shared across threads.

Crucially, we **retain** the existing single-threaded `GcCell` for high-performance, contention-free use cases. This split adheres to Rust's distinct `Cell` (single-threaded) vs `Lock` (multi-threaded) paradigms.

## 2. Design Philosophy

### 2.1. Separation of Concerns
-   **`GcCell<T>`**: Remains `!Sync`. Uses `RefCell`. Fastest for thread-local logic (e.g., DOM trees, compiler ASTs).
-   **`GcRwLock<T>`**: New. `Sync + Send`. Uses `parking_lot::RwLock`. Optimized for read-heavy concurrent workloads (e.g., configurations, global caches).
-   **`GcMutex<T>`**: New. `Sync + Send`. Uses `parking_lot::Mutex`. Optimized for write-heavy concurrent workloads (e.g., shared queues, state machines).

### 2.2. The "Stop-The-World" Lock Bypass
Garbage Collection (marking) occurs during a global Stop-The-World (STW) pause.
-   **Problem**: A mutator thread might be paused while holding a lock. If the GC thread attempts to acquire that lock to trace the object, it inevitably deadlocks.
-   **Solution**: The `Trace` implementation for concurrent primitives must **bypass the lock**.
-   **Safety Proof**:
    1.  **Atomicity**: In Rust, logical "writes" to pointer fields are atomic CPU instructions. A thread suspended at a safepoint is not "halfway" through writing a pointer.
    2.  **Exclusivity**: During STW, all mutator threads are suspended. No thread is actively mutating memory. Therefore, it is safe for the GC thread to read the underlying data without acquiring the lock.
    3.  **Visibility**: STW synchronization barriers ensure memory visibility.

## 3. Technical Specification

### 3.1. `GcRwLock<T>`

Wraps `parking_lot::RwLock` to provide a concurrent reader-writer lock.

```rust
use parking_lot::RwLock;

pub struct GcRwLock<T: ?Sized> {
    inner: RwLock<T>,
}

// Thread Safety: Dependent on T being Thread Safe
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcRwLock<T> {}
unsafe impl<T: Trace + Send + ?Sized> Send for GcRwLock<T> {}
```

#### API
-   `read()` -> `GcRwLockReadGuard`: RAII guard for reading.
-   `write()` -> `GcRwLockWriteGuard`: RAII guard for writing. Triggers **write barriers** (Generational + SATB) immediately upon acquisition.

#### Trace Implementation (The Bypass)
```rust
unsafe impl<T: Trace + ?Sized> Trace for GcRwLock<T> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // SAFETY: 
        // 1. GC runs in STW. All mutators are paused.
        // 2. data_ptr() returns a raw pointer to the contents.
        // 3. We read without locking to avoid deadlock with paused threads.
        let raw_ptr = self.inner.data_ptr();
        unsafe { (*raw_ptr).trace(visitor); }
    }
}
```

### 3.2. `GcMutex<T>`

Wraps `parking_lot::Mutex` for exclusive locking.

```rust
use parking_lot::Mutex;

pub struct GcMutex<T: ?Sized> {
    inner: Mutex<T>,
}

unsafe impl<T: Trace + Send + ?Sized> Sync for GcMutex<T> {}
unsafe impl<T: Trace + Send + ?Sized> Send for GcMutex<T> {}
```

#### API
-   `lock()` -> `GcMutexGuard`: RAII guard. Triggers **write barriers**.

#### Trace Implementation
`parking_lot::Mutex` also provides `data_ptr()` (via `RawMutex` internals or similar mechanism), allowing unsafe access to the underlying data during STW.

### 3.3. Write Barriers

Write barriers must be applied when a mutable guard (`write` or `lock`) is acquired.

1.  **Generational Barrier**:
    -   Requires adding the object's page to the current thread's dirty list.
    -   **Concurrency**: Thread-safe because dirty lists are thread-local and `PageHeader` bitmaps are atomic.

2.  **SATB Barrier (Incremental)**:
    -   Requires reading the *old value* before modification.
    -   **Concurrency**: Safe because the thread holding the write lock has exclusive access to read the old value.

### 3.4. Module Structure

```
crates/rudo-gc/src/
├── cell.rs        (Keeps GcCell, RefCell logic)
└── sync.rs        (New module: GcRwLock, GcMutex)
```

## 4. Implementation Plan

1.  **Create `src/sync.rs`**:
    -   Implement `GcRwLock` using `parking_lot::RwLock`.
    -   Implement `GcMutex` using `parking_lot::Mutex`.
2.  **Implement `Trace`**:
    -   Use `data_ptr()` (exposed by `parking_lot`'s raw lock API) to implement the "Lock Bypass".
3.  **Implement Barriers**:
    -   Reuse barrier logic from `cell.rs` (extract common logic if possible, or duplicate for `sync` context).
4.  **Expose in `lib.rs`**: 
    -   Re-export `GcRwLock` and `GcMutex` at the crate root.

## 5. Migration Guide

-   **Single-threaded users** (e.g. DOM, scripting): Continue using `GcCell`. No changes needed.
-   **Multi-threaded users**: Use `Gc<GcRwLock<T>>` or `Gc<GcMutex<T>>`.

## 6. Risks & Mitigations

-   **Deadlocks**: If a user manually implements `Trace` and tries to re-acquire the lock inside `trace()`, they will deadlock. **Mitigation**: Documentation warning; `Trace` is usually derived, so this is rare.
-   **Mapped Guards**: `parking_lot` supports `RwLockReadGuard::map`. We must ensure we don't accidentally expose non-GC-safe pointers or bypass barriers. **Mitigation**: Only expose basic locking APIs initially.

## 7. Performance Considerations

-   `GcCell`: ~0 overhead (non-atomic).
-   `GcRwLock`/`GcMutex`: ~Atomic overhead.
-   **Conclusion**: Splitting the types ensures users only pay for synchronization when they actually need it.
