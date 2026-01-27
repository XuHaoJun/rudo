# Data Model: Send + Sync Trait Support

**Date**: 2026-01-27  
**Feature**: Send + Sync Trait Support (`002-send-sync-trait`)

---

## Entities

### GcBox<T>

The internal heap allocation containing the user's value and metadata.

| Field | Type | Atomicity | Description |
|-------|------|-----------|-------------|
| `ref_count` | `AtomicUsize` | Atomic | Strong reference count |
| `weak_count` | `AtomicUsize` | Atomic | Weak reference count |
| `drop_fn` | `unsafe fn(*mut u8)` | N/A | Type-erased destructor |
| `trace_fn` | `unsafe fn(*const u8, &mut GcVisitor)` | N/A | Type-erased tracer |
| `value` | `T` | N/A | User data |

**Constraints**:
- `ref_count` saturates at `isize::MAX`
- `weak_count` must be <= `ref_count + 1` when value is alive

### Gc<T>

Smart pointer providing shared ownership with garbage collection.

| Field | Type | Atomicity | Description |
|-------|------|-----------|-------------|
| `ptr` | `AtomicPtr<GcBox<T>>` | Atomic | Pointer to GcBox |

**Trait Bounds**: `T: Trace + ?Sized + 'static`

### Weak<T>

Weak reference that does not prevent collection.

| Field | Type | Atomicity | Description |
|-------|------|-----------|-------------|
| `ptr` | `AtomicPtr<GcBox<T>>` | Atomic | Pointer to GcBox |

**Trait Bounds**: `T: Trace + ?Sized + 'static`

---

## Operations

### GcBox Operations

| Operation | Atomicity | Memory Ordering | Description |
|-----------|-----------|-----------------|-------------|
| `inc_ref()` | Atomic | `Relaxed` | Increment strong count |
| `dec_ref()` | Atomic | `AcqRel` | Decrement strong count, returns true if last |
| `inc_weak()` | Atomic | `Relaxed` | Increment weak count |
| `dec_weak()` | Atomic | `AcqRel` | Decrement weak count, returns true if last |

### Gc<T> Operations

| Operation | Atomicity | Memory Ordering | Thread Safety |
|-----------|-----------|-----------------|---------------|
| `new(value: T)` | N/A | N/A | Thread-local only |
| `clone(&self)` | Atomic | `Acquire` (load), `Release` (store) | Thread-safe |
| `deref(&self)` | Atomic | `Acquire` | Thread-safe |
| `downgrade(&self)` | Atomic | `Acquire`/`Release` | Thread-safe |

### Weak<T> Operations

| Operation | Atomicity | Memory Ordering | Thread Safety |
|-----------|-----------|-----------------|---------------|
| `upgrade(&self)` | Atomic | `Acquire` | Thread-safe |
| `clone(&self)` | Atomic | `Relaxed` | Thread-safe |
| `is_alive(&self)` | Atomic | `Relaxed` | Thread-safe |

---

## Trait Implementations

### New Trait Bounds

```rust
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for Gc<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for Gc<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for Weak<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for Weak<T> {}
```

### Existing Trait Implementations (Unchanged)

- `Deref` for `Gc<T>` and `Weak<T>`
- `Clone` for `Gc<T>` and `Weak<T>`
- `Drop` for `Gc<T>` and `Weak<T>`
- `Trace` for `Gc<T>` and `Weak<T>`
- `PartialEq`, `Eq`, `Debug`, `Display` for `Gc<T>`

---

## State Transitions

### GcBox Lifetime States

```
┌─────────────────┐     inc_ref()      ┌──────────────────┐
│   ALIVE (rc=n)  │ ────────────────► │ ALIVE (rc=n+1)   │
 └─────────────────┘                    └──────────────────┘
         │                                      │
         │ dec_ref()                            │ dec_ref()
         ▼                                      ▼
┌─────────────────┐                    ┌──────────────────┐
│   VALUE_DEAD    │                    │   RELEASED       │
│   (rc=1, weak>0)│                    │   (rc=0)         │
 └─────────────────┘                    └──────────────────┘
         │                                      │
         │ inc_ref()                            │ dec_weak()
         ▼                                      ▼
┌─────────────────┐                    ┌──────────────────┐
│   ALIVE (rc=2)  │                    │   DEALLOCATED    │
└─────────────────┘                    └──────────────────┘
```

---

## Validation Rules

1. **Reference Count Invariant**: `ref_count >= 1` while value is alive
2. **Weak Count Invariant**: `weak_count >= 0`, can be non-zero when value is dead
3. **Pointer Validity**: `ptr` is never dereferenced when null (except during Drop)
4. **Memory Ordering**: All atomic operations use specified ordering

---

## Relationships

```
Gc<T> ──points to──► GcBox<T> ──contains──► T (user value)
 │                                    ▲
 └───can downgrade to──► Weak<T> ─────┘
```

- Multiple `Gc<T>` can point to the same `GcBox<T>`
- Multiple `Weak<T>` can point to the same `GcBox<T>`
- `GcBox<T>` contains exactly one `T` value
