# Data Model: Concurrent GC Primitives

**Feature**: 011-concurrent-gc-primitives | **Date**: 2026-02-08

## Type Hierarchy

```
Gc<T> (base pointer type)
├── GcCell<T> (single-threaded, !Sync)
├── GcRwLock<T> (multi-threaded, Sync + Send, read-heavy)
└── GcMutex<T> (multi-threaded, Sync + Send, write-heavy)
```

## Entity: GcRwLock<T>

Reader-writer lock wrapper for GC objects, optimized for read-heavy concurrent workloads.

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `inner` | `parking_lot::RwLock<T>` | Inner lock protecting the data |

### Traits

- `Debug`: Delegates to inner RwLock's Debug impl
- `Trace`: Bypasses lock during GC tracing (see below)
- `Send`: When `T: Trace + Send + Sync + ?Sized`
- `Sync`: When `T: Trace + Send + Sync + ?Sized`

### Guard Types

| Guard | Access | Barrier Trigger |
|-------|--------|-----------------|
| `GcRwLockReadGuard` | Shared read | None (read-only) |
| `GcRwLockWriteGuard` | Exclusive write | Generational + SATB |

### API Methods

```rust
impl<T: ?Sized> GcRwLock<T> {
    pub fn read(&self) -> GcRwLockReadGuard<'_, T>
    pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
    pub fn try_read(&self) -> Option<GcRwLockReadGuard<'_, T>>
    pub fn try_write(&self) -> Option<GcRwLockWriteGuard<'_, T>>
    pub fn is_locked(&self) -> bool
}
```

## Entity: GcMutex<T>

Exclusive mutex wrapper for GC objects, optimized for write-heavy concurrent workloads.

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `inner` | `parking_lot::Mutex<T>` | Inner lock protecting the data |

### Traits

- `Debug`: Delegates to inner Mutex's Debug impl
- `Trace`: Bypasses lock during GC tracing
- `Send`: When `T: Trace + Send + Sync + ?Sized`
- `Sync`: When `T: Trace + Send + Sync + ?Sized`

### Guard Types

| Guard | Access | Barrier Trigger |
|-------|--------|-----------------|
| `GcMutexGuard` | Exclusive | Generational + SATB |

### API Methods

```rust
impl<T: ?Sized> GcMutex<T> {
    pub fn lock(&self) -> GcMutexGuard<'_, T>
    pub fn try_lock(&self) -> Option<GcMutexGuard<'_, T>>
    pub fn is_locked(&self) -> bool
}
```

## Trace Implementation (Lock Bypass)

```rust
unsafe impl<T: Trace + ?Sized> Trace for GcRwLock<T> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // SAFETY:
        // 1. GC runs in STW pause - all mutators suspended
        // 2. data_ptr() returns raw pointer to inner data
        // 3. No other thread can write during STW - safe to read without lock
        let raw_ptr = self.inner.data_ptr();
        unsafe { (*raw_ptr).trace(visitor); }
    }
}

unsafe impl<T: Trace + ?Sized> Trace for GcMutex<T> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // SAFETY: Same rationale as GcRwLock
        let raw_ptr = self.inner.data_ptr();
        unsafe { (*raw_ptr).trace(visitor); }
    }
}
```

## Comparison: GcCell vs GcRwLock vs GcMutex

| Characteristic | GcCell | GcRwLock | GcMutex |
|---------------|--------|----------|---------|
| Threading | !Sync | Sync + Send | Sync + Send |
| Inner primitive | RefCell | parking_lot::RwLock | parking_lot::Mutex |
| Reader overhead | None (copy) | Atomic | N/A |
| Writer overhead | None (RefCell) | Atomic + barrier | Atomic + barrier |
| Multiple readers | No | Yes | No |
| Use case | DOM, AST, scripting | Caches, configs | Queues, state machines |
| GC tracing | Direct | Bypass lock | Bypass lock |

## Module Structure

```
crates/rudo-gc/src/
├── lib.rs           # Re-exports: pub use sync::{GcRwLock, GcMutex}
├── cell.rs          # GcCell (existing, unchanged)
└── sync.rs          # NEW: GcRwLock, GcMutex, guard types
```