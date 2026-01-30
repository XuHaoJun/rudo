# Data Model: Tokio Async/Await Integration

**Feature**: 004-tokio-async-integration  
**Date**: 2026-01-30

## Entities

### GcRootSet

Process-level singleton that maintains the collection of active GC roots across all tokio tasks and runtimes.

```rust
pub struct GcRootSet {
    roots: Mutex<Vec<usize>>,
    count: AtomicUsize,
    dirty: AtomicBool,
}
```

| Field | Type | Validation | Description |
|-------|------|------------|-------------|
| roots | Mutex<Vec<usize>> | Unique pointers only | Collection of root pointers |
| count | AtomicUsize | Always >= 0 | Number of active roots |
| dirty | AtomicBool | N/A | Flag set when roots change |

**Relationships**: Singleton accessed via `GcRootSet::global()`

**State machine**:
```
clean (dirty=false) --register()--> dirty (dirty=true)
dirty (dirty=true) --snapshot() + clear_dirty()--> clean (dirty=false)
```

### GcRootGuard

RAII guard that registers a Gc pointer on creation and unregisters it on drop.

```rust
#[must_use]
pub struct GcRootGuard {
    ptr: usize,
    _phantom: PhantomData<u8>,
}
```

| Field | Type | Description |
|-------|------|-------------|
| ptr | usize | Address of guarded Gc pointer |
| _phantom | PhantomData<u8> | Asserts correct type semantics |

**Relationships**: Created via `Gc::root_guard()`, owns registration in GcRootSet

**Lifecycle**:
```
new --(register ptr in GcRootSet)--> active
active --(Drop)--> (unregister ptr from GcRootSet)
```

### GcRootScope<F>

Future wrapper that holds both the wrapped future and a root guard, ensuring automatic root tracking for spawned tasks.

```rust
struct GcRootScope<F> {
    future: F,
    _guard: GcRootGuard,
}
```

| Field | Type | Description |
|-------|------|-------------|
| future | F | The wrapped future |
| _guard | GcRootGuard | Owned guard for root tracking |

**Relationships**: Wraps futures passed to `gc::spawn()`

### GcTokioExt

Trait extension for `Gc<T>` providing tokio-specific methods when tokio feature is enabled.

```rust
#[cfg(feature = "tokio")]
pub trait GcTokioExt: Trace + Send + Sync {
    fn root_guard(&self) -> GcRootGuard;
    async fn yield_now(&self);
}
```

**Implementation**: Available for all `T: Trace + Send + Sync`

## Validation Rules

### GcRootSet

1. **Duplicate prevention**: `register()` must not add duplicate pointers
2. **Count accuracy**: `count()` must reflect actual number of roots
3. **Dirty flag**: Must be set on any root modification

```rust
impl GcRootSet {
    pub fn register(&self, ptr: usize) {
        let mut roots = self.roots.lock().unwrap();
        if !roots.contains(&ptr) {
            roots.push(ptr);
        }
        drop(roots);
        self.count.fetch_add(1, Ordering::AcqRel);
        self.dirty.store(true, Ordering::Release);
    }
}
```

### GcRootGuard

1. **#[must_use]**: Guards must not be silently dropped
2. **Single registration**: Each guard registers exactly one pointer
3. **Automatic cleanup**: Drop unregisters the pointer

## State Transitions

### GcRootSet

```
                    +----------------+
                    |     clean      | <------------------+
                    | dirty = false  |                   |
                    +----------------+                   |
                             ^                           |
                             | register/unregister      |
                             |                           |
                    +----------------+                   |
                    |     dirty      | -----------------+
                    | dirty = true   |
                    +----------------+
                             |
                    snapshot() + clear_dirty()
                             |
                             v
                    +----------------+
                    |     clean      |
                    | dirty = false  |
                    +----------------+
```

### GcRootGuard

```
   new()                                    Drop
    |                                         |
    v                                         v
+-------+  register()  +--------+  unregister()  +--------+
| Guard | ----------> | Active | -------------> | Done   |
+-------+             +--------+                +--------+
                      ptr in GcRootSet          ptr removed
```

## Entity Relationships

```
GcRootSet (singleton)
    |
    +-- registers --> GcRootGuard (one-to-many)
    |                     |
    |                     +-- owns --> Gc<T> pointer
    |
    +-- wraps --> GcRootScope<F>
                          |
                          +-- contains --> future: F
                          +-- owns --> _guard: GcRootGuard

GcTokioExt (trait)
    |
    +-- implemented for --> Gc<T>
          |
          +-- provides --> root_guard() -> GcRootGuard
          +-- provides --> yield_now() -> impl Future
```

## Key Invariants

1. **Root set consistency**: For every registered root, exactly one active guard exists
2. **No dangling pointers**: Guard drop always precedes GC collection of the object
3. **Dirty flag accuracy**: Flag is true iff roots have changed since last snapshot
4. **Count accuracy**: `count()` equals number of active guards
