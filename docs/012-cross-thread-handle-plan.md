# Feature 012: Cross-Thread GcHandle — Implementation Plan

**Feature Number:** 012  
**Status:** Approved (revised after review)  
**Date:** 2026-02-10  
**Author:** Technical Review  
**Reviewers:** R. Kent Dybvig, Rust Leadership Council  
**Target Crate:** `rudo-gc`

---

## Overview

Implement a cross-thread handle system for rudo-gc that allows safe hand-off of GC-managed object references between threads. This feature enables frameworks like Rvue to schedule UI updates from async threads without requiring the signal types themselves to be `Send + Sync`.

---

## Problem Statement

**Current Limitation:**
- `Gc<T>` is `Send + Sync` only when `T: Trace + Send + Sync`
- Signal inner types contain `Weak<Effect>` (which is `!Send`), so `Gc<SignalDataInner<T>>` cannot be sent across threads
- Rvue needs to send signal handles from async threads to the UI thread for updates

**Desired Solution:**
- A `Send + Sync` handle type that can cross threads
- Resolution only on the origin thread into a local `Gc<T>`
- Strong semantics: handle keeps object alive until dropped or unregistered

---

## Safety Invariants

The core safety argument for `GcHandle<T>` being `Send + Sync` even when `T: !Send`:

1. **No direct access to `T` from non-origin threads.** The handle is an opaque
   token — it stores no reference through which `T` can be read or written.
   The only way to obtain a `Gc<T>` (and thus access `T`) is via `resolve()`,
   which enforces origin-thread affinity at runtime.

2. **Origin-thread enforcement is a hard check, not advisory.** `resolve()`
   compares `std::thread::current().id()` against the stored `origin_thread`.
   This is a panic, not UB — the invariant is enforced before any access to `T`.

3. **Root registration keeps the object alive.** The handle holds an
   `Arc<ThreadControlBlock>` and the root entry is stored in the TCB's
   `Mutex`-protected handle list. This means:
   - The GC will always find and mark the referenced object during root scanning.
   - The root entry outlives the origin thread's stack (because the `Arc` prevents
     TCB deallocation).
   - Drop from any thread is safe: it only needs to lock the TCB mutex to remove
     its entry — no thread-local storage access required.

4. **`WeakCrossThreadHandle<T>` also enforces origin-thread affinity on
   `resolve()`** — because resolving produces a `Gc<T>`, which for `T: !Send`
   must not exist on a foreign thread.

5. **`'static` bound is required.** Handles may outlive the scope in which the
   object was allocated. The `'static` bound prevents dangling lifetime
   references inside `T`.

---

## API Design

### Primary Types

```rust
/// Strong cross-thread handle — keeps the referenced object alive.
///
/// Created via `Gc::cross_thread_handle()`. The handle is `Send + Sync`
/// regardless of whether `T` is, because it never exposes `T` directly.
/// Resolution back to `Gc<T>` is only permitted on the origin thread.
pub struct GcHandle<T: Trace + 'static> {
    /// Raw pointer to the GcBox. Validity is guaranteed by root registration.
    ptr: NonNull<GcBox<T>>,
    /// TCB of the origin thread. Prevents TCB deallocation; holds root list.
    origin_tcb: Arc<ThreadControlBlock>,
    /// Origin thread identity, for resolve-time check.
    origin_thread: ThreadId,
    /// Unique ID for this handle's root entry (for O(1) unregistration).
    handle_id: HandleId,
}

/// Weak cross-thread handle — does not prevent collection.
///
/// Created via `Gc::weak_cross_thread_handle()`. Like `GcHandle`, the handle
/// is `Send + Sync` but resolve is origin-thread-only (because `T` may be `!Send`).
pub struct WeakCrossThreadHandle<T: Trace + 'static> {
    weak: GcBoxWeakRef<T>,
    origin_tcb: Arc<ThreadControlBlock>,
    origin_thread: ThreadId,
}

/// Opaque ID for a registered cross-thread handle root entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct HandleId(u64);
```

### Gc Extension Methods

```rust
impl<T: Trace + 'static> Gc<T> {
    /// Creates a cross-thread handle to this GC object.
    ///
    /// The handle is `Send + Sync` and can be sent to any thread.
    /// Call `resolve()` on the creating thread to obtain a local `Gc<T>`.
    ///
    /// The object will not be collected while any strong handle to it exists.
    pub fn cross_thread_handle(&self) -> GcHandle<T> { ... }

    /// Creates a weak cross-thread handle that doesn't prevent collection.
    ///
    /// Resolve returns `None` if the object has been collected.
    pub fn weak_cross_thread_handle(&self) -> WeakCrossThreadHandle<T> { ... }
}
```

### Handle Methods

```rust
impl<T: Trace + 'static> GcHandle<T> {
    /// Returns the thread where this handle was created.
    pub fn origin_thread(&self) -> ThreadId;

    /// Resolves this handle to a `Gc<T>` on the origin thread.
    ///
    /// # Panics
    /// Panics if called from a thread other than the origin thread.
    pub fn resolve(&self) -> Gc<T>;

    /// Tries to resolve, returning `None` if called from wrong thread.
    ///
    /// Useful in contexts where you cannot guarantee which thread you're on.
    pub fn try_resolve(&self) -> Option<Gc<T>>;

    /// Returns `true` if the underlying object is still alive.
    ///
    /// For strong handles this is always `true` while the handle is registered,
    /// unless the origin thread's heap has been torn down.
    pub fn is_valid(&self) -> bool;

    /// Downgrades to a weak cross-thread handle.
    pub fn downgrade(&self) -> WeakCrossThreadHandle<T>;

    /// Explicitly unregisters this handle from the root set.
    ///
    /// After unregistration, `resolve()` will panic and `is_valid()` returns false.
    /// The object becomes eligible for collection (unless other roots exist).
    ///
    /// This is idempotent — calling it multiple times is safe.
    pub fn unregister(&mut self);
}

impl<T: Trace + 'static> WeakCrossThreadHandle<T> {
    /// Returns the thread where this handle was created.
    pub fn origin_thread(&self) -> ThreadId;

    /// Resolves to a `Gc<T>` if the object is still alive.
    ///
    /// # Panics
    /// Panics if called from a thread other than the origin thread,
    /// because `T` may be `!Send`.
    pub fn resolve(&self) -> Option<Gc<T>>;

    /// Tries to resolve, returning `None` if called from wrong thread
    /// or if the object has been collected.
    pub fn try_resolve(&self) -> Option<Gc<T>>;

    /// Returns `true` if the underlying object is still alive.
    /// Can be called from any thread (doesn't access `T`).
    pub fn is_valid(&self) -> bool;
}
```

### Clone Semantics

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    /// Cloning a strong handle registers an additional root entry.
    ///
    /// Each clone independently keeps the object alive. Dropping one clone
    /// does not affect the others.
    fn clone(&self) -> Self { ... }
}

impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    fn clone(&self) -> Self { ... }
}
```

### Drop Semantics

```rust
impl<T: Trace + 'static> Drop for GcHandle<T> {
    /// Unregisters the root entry from the origin thread's TCB.
    ///
    /// Safe to call from any thread: the TCB is held via `Arc`, and the root
    /// list is `Mutex`-protected. No thread-local storage is accessed.
    fn drop(&mut self) { ... }
}
```

### Thread Safety

```rust
// SAFETY: GcHandle never exposes T directly. The only path to T is resolve(),
// which enforces origin-thread affinity. The handle's internal state (ptr, Arc<TCB>,
// ThreadId, HandleId) is all Send+Sync-safe. Root registration/unregistration goes
// through a Mutex on the TCB, which is safe from any thread.
unsafe impl<T: Trace + 'static> Send for GcHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for GcHandle<T> {}

// SAFETY: Same argument as GcHandle. WeakCrossThreadHandle additionally never
// prevents collection, and is_valid() reads only atomic/GC metadata.
unsafe impl<T: Trace + 'static> Send for WeakCrossThreadHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for WeakCrossThreadHandle<T> {}
```

### Standard Trait Implementations

```rust
impl<T: Trace + 'static> Debug for GcHandle<T> { ... }
impl<T: Trace + 'static> Debug for WeakCrossThreadHandle<T> { ... }
```

---

## Implementation Details

### 1. Cross-Thread Handle Root Storage (on `ThreadControlBlock`)

Handles are stored on the origin thread's `ThreadControlBlock`, not in
thread-local `LocalRoots`. This is the key design decision that makes
cross-thread `Drop` safe.

```rust
/// Added to ThreadControlBlock:
struct ThreadControlBlock {
    // ... existing fields ...

    /// Root entries for cross-thread handles. Protected by Mutex so that
    /// handles can be registered/unregistered from any thread.
    cross_thread_roots: Mutex<CrossThreadRootTable>,
}

struct CrossThreadRootTable {
    /// Monotonically increasing ID counter.
    next_id: u64,
    /// Strong handle root entries: maps HandleId -> raw GcBox pointer.
    /// These are treated as roots during GC marking.
    strong: HashMap<HandleId, NonNull<GcBox<()>>>,
}
```

**Why `ThreadControlBlock` and not `LocalRoots`?**

`LocalRoots` is accessed through thread-local storage and is not safely
accessible from other threads. `ThreadControlBlock` is `Arc`-shared and
already used for cross-thread coordination (safepoints, stack roots, etc.).
The `Mutex` has minimal contention: it is only held briefly during
handle creation, clone, drop, and GC root scanning.

### 2. Handle Creation (Atomicity Guarantee)

Handle creation must be atomic with respect to GC — the object must not be
collected between obtaining the pointer and registering the root. We achieve
this by performing both operations while holding the TCB's root table lock:

```rust
impl<T: Trace + 'static> Gc<T> {
    pub fn cross_thread_handle(&self) -> GcHandle<T> {
        let tcb = current_thread_tcb();

        // Lock the root table BEFORE reading the pointer.
        // While this lock is held, GC cannot sweep this thread's
        // cross-thread roots (GC also acquires this lock for marking).
        let mut roots = tcb.cross_thread_roots.lock();
        let handle_id = roots.allocate_id();

        let ptr = self.as_non_null();
        roots.strong.insert(handle_id, ptr.cast::<GcBox<()>>());

        drop(roots); // Release lock.

        GcHandle {
            ptr,
            origin_tcb: Arc::clone(&tcb),
            origin_thread: std::thread::current().id(),
            handle_id,
        }
    }
}
```

> **Note (Dybvig):** The critical invariant is that no GC cycle can observe a
> state where the pointer has been read but the root is not yet registered.
> Holding the TCB lock across both operations establishes this. The GC's root
> scanning phase acquires the same lock, creating the necessary
> happens-before relationship.

### 3. Drop from Any Thread

```rust
impl<T: Trace + 'static> Drop for GcHandle<T> {
    fn drop(&mut self) {
        // Lock the origin thread's root table. This is safe from any thread
        // because origin_tcb is an Arc<ThreadControlBlock>.
        let mut roots = self.origin_tcb.cross_thread_roots.lock();
        roots.strong.remove(&self.handle_id);
        // Lock released here. The object becomes eligible for collection
        // on the next GC cycle (unless other roots exist).
    }
}
```

**No thread-local storage access. No origin-thread affinity requirement.**
The `Arc<ThreadControlBlock>` keeps the TCB alive even if the origin thread
has exited. The `Mutex` provides safe concurrent access.

### 4. GC Integration — Root Marking

During the mark phase, the collector iterates cross-thread handle root
entries alongside existing roots:

```rust
fn mark_cross_thread_roots(tcb: &ThreadControlBlock, visitor: &mut GcVisitor) {
    let roots = tcb.cross_thread_roots.lock();

    for (_id, ptr) in &roots.strong {
        // Strong handles are roots — mark the object as reachable.
        // SAFETY: ptr validity is guaranteed because the handle registered
        // it before releasing the lock, and the GC holds the lock now,
        // so no concurrent Drop can remove it mid-iteration.
        unsafe {
            visitor.mark(*ptr);
        }
    }
}

// Called during root scanning (gc/gc.rs):
fn mark_all_roots(registry: &ThreadRegistry, visitor: &mut GcVisitor) {
    for tcb in registry.threads() {
        // ... existing root scanning ...
        mark_cross_thread_roots(tcb, visitor);
    }
}
```

> **Note (Dybvig):** Use `mark()` (which adds to the worklist), not
> `mark_gray()`. Cross-thread handle roots are strong roots, semantically
> identical to stack roots. They must be fully traced, not merely shaded.

### 5. Resolve Implementation

```rust
impl<T: Trace + 'static> GcHandle<T> {
    pub fn resolve(&self) -> Gc<T> {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "GcHandle::resolve() must be called on the origin thread \
             (origin={:?}, current={:?})",
            self.origin_thread,
            std::thread::current().id(),
        );

        // SAFETY: The root registration guarantees the object is alive.
        // We've verified we're on the origin thread, so producing a Gc<T>
        // is safe even if T: !Send.
        unsafe { Gc::from_raw(self.ptr) }
    }

    pub fn try_resolve(&self) -> Option<Gc<T>> {
        if std::thread::current().id() != self.origin_thread {
            return None;
        }
        // SAFETY: same as resolve().
        Some(unsafe { Gc::from_raw(self.ptr) })
    }
}

impl<T: Trace + 'static> WeakCrossThreadHandle<T> {
    pub fn resolve(&self) -> Option<Gc<T>> {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "WeakCrossThreadHandle::resolve() must be called on the origin thread"
        );

        // Weak handle does not prevent collection. Check liveness first.
        self.weak.upgrade()
    }

    pub fn try_resolve(&self) -> Option<Gc<T>> {
        if std::thread::current().id() != self.origin_thread {
            return None;
        }
        self.weak.upgrade()
    }
}
```

### 6. Interaction with Incremental Marking (Feature 008)

When a cross-thread handle is resolved during an active incremental marking
phase, the resulting `Gc<T>` may be stored into an already-marked object.
This is handled by the existing write barrier in `GcCell::borrow_mut()`:

- **SATB barrier** captures the old pointer value before mutation.
- **Dijkstra insertion barrier** marks the new pointer value (the resolved
  `Gc<T>`) immediately.

No additional barriers are needed in `GcHandle::resolve()` itself, because
`resolve()` only produces a local `Gc<T>` — it does not perform a store.
The barrier fires when the user subsequently writes the `Gc<T>` into a
`GcCell`, which is the correct interposition point.

> **Note (Dybvig):** This is the right design. Barriers belong at stores,
> not at reads. Placing a barrier in `resolve()` would be both unnecessary
> and expensive (it runs on the hot path of every resolution).

### 7. Thread Exit Behavior

**Origin thread exit while handles exist:**

Because `GcHandle` holds `Arc<ThreadControlBlock>`, the TCB is not deallocated
when the origin thread exits. The root entries remain valid and the GC will
continue to mark them during collection.

However, the origin thread's `LocalHeap` may be torn down. This means:

1. **`resolve()` will still panic** — the origin thread no longer exists, so
   the `ThreadId` check will fail (no thread can match).
2. **The referenced object remains alive** — root entries in the TCB prevent
   collection. The object will be collected when the last `GcHandle` is dropped.
3. **`is_valid()` remains correct** — it checks the GC metadata, not thread
   liveness.
4. **`Drop` remains safe** — it only accesses the `Arc<TCB>`'s mutex, not
   thread-local storage.

**Documented behavior:** If the origin thread has exited, `resolve()` will
always panic. Users should ensure handles are resolved or dropped before the
origin thread exits. The `try_resolve()` method returns `None` in this case,
enabling graceful handling.

> **Note (Dybvig):** This avoids the classic guardian/weak-reference
> dangling problem. By anchoring roots to the TCB (which outlives the thread
> via Arc), we sidestep the need for a finalizer-style cleanup protocol.
> Objects simply float as unreachable-but-rooted until the handles are dropped.

### 8. Unregister Semantics

```rust
impl<T: Trace + 'static> GcHandle<T> {
    pub fn unregister(&mut self) {
        let mut roots = self.origin_tcb.cross_thread_roots.lock();
        roots.strong.remove(&self.handle_id);
        // Mark as unregistered so Drop is a no-op and resolve panics.
        self.handle_id = HandleId::INVALID;
    }

    pub fn is_valid(&self) -> bool {
        self.handle_id != HandleId::INVALID
    }
}

impl HandleId {
    const INVALID: HandleId = HandleId(u64::MAX);
}
```

`unregister()` is idempotent. After unregistration:
- `resolve()` panics (handle is no longer valid).
- `Drop` is a no-op (the entry was already removed).
- `is_valid()` returns `false`.

### 9. Clone Implementation

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        assert_ne!(
            self.handle_id,
            HandleId::INVALID,
            "cannot clone an unregistered GcHandle"
        );

        let mut roots = self.origin_tcb.cross_thread_roots.lock();
        let new_id = roots.allocate_id();
        roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());

        GcHandle {
            ptr: self.ptr,
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
            handle_id: new_id,
        }
    }
}
```

Each clone independently registers a root. Dropping one clone does not
affect liveness of others.

---

## File Structure

```
crates/rudo-gc/src/
├── handles/
│   ├── mod.rs               # Export cross_thread module
│   └── cross_thread.rs      # GcHandle, WeakCrossThreadHandle, HandleId (NEW)
├── heap.rs                   # ThreadControlBlock additions
├── gc/
│   └── gc.rs                 # mark_cross_thread_roots() integration
├── ptr.rs                    # Gc::cross_thread_handle() methods
└── lib.rs                    # Exports

tests/
└── cross_thread_handle.rs    # Integration tests (NEW)
```

---

## Implementation Steps

### Step 1: Core Infrastructure (`heap.rs`)
- [ ] Add `CrossThreadRootTable` struct with `HashMap<HandleId, NonNull<GcBox<()>>>`
- [ ] Add `HandleId` type with `INVALID` sentinel
- [ ] Add `cross_thread_roots: Mutex<CrossThreadRootTable>` to `ThreadControlBlock`
- [ ] Implement `allocate_id()` on `CrossThreadRootTable`

### Step 2: Core Types (`handles/cross_thread.rs`)
- [ ] Define `GcHandle<T>` struct (ptr, origin_tcb, origin_thread, handle_id)
- [ ] Define `WeakCrossThreadHandle<T>` struct (weak, origin_tcb, origin_thread)
- [ ] Implement `Send + Sync` with `// SAFETY` comments for both
- [ ] Implement `Drop` for `GcHandle` (mutex-based, any-thread safe)
- [ ] Implement `Clone` for both types
- [ ] Implement `Debug` for both types
- [ ] Implement `origin_thread()`, `is_valid()`, `unregister()` on `GcHandle`
- [ ] Implement `resolve()` and `try_resolve()` on both types

### Step 3: Gc Extension Methods (`ptr.rs`)
- [ ] Add `Gc::cross_thread_handle()` — atomic registration under lock
- [ ] Add `Gc::weak_cross_thread_handle()`
- [ ] Add `GcHandle::downgrade()` method

### Step 4: GC Integration (`gc/gc.rs`)
- [ ] Add `mark_cross_thread_roots()` function
- [ ] Call it from `mark_all_roots()` during root scanning phase
- [ ] Ensure lock ordering is documented: cross_thread_roots lock is acquired
      *after* LocalHeap lock (extends existing Heap → GlobalMarkState → Request order)

### Step 5: Module Exports (`handles/mod.rs`, `lib.rs`)
- [ ] Export `GcHandle` and `WeakCrossThreadHandle` types
- [ ] Add to public API documentation

### Step 6: Tests
- [ ] `test_cross_thread_send`: Handle sent between threads via channel
- [ ] `test_resolve_origin_thread`: Verify `resolve()` panics on wrong thread
- [ ] `test_try_resolve_wrong_thread`: Verify `try_resolve()` returns None
- [ ] `test_handle_keeps_alive`: Verify object not collected while handle exists
- [ ] `test_weak_handle_no_prevent`: Verify weak handle doesn't prevent collection
- [ ] `test_is_valid_checks`: Verify `is_valid()` reflects registration state
- [ ] `test_clone_independent_lifetime`: Cloned handles are independent roots
- [ ] `test_unregister_idempotent`: Double unregister is safe
- [ ] `test_drop_from_foreign_thread`: Drop on non-origin thread is safe
- [ ] `test_multiple_handles_same_object`: Multiple handles to same object
- [ ] `test_origin_thread_exit`: Behavior when origin thread exits
- [ ] `test_downgrade`: Strong-to-weak downgrade
- [ ] `test_miri_thread_safety`: Miri verification for unsafe code

### Step 7: Documentation
- [ ] Update `AGENTS.md` with new feature
- [ ] Add API documentation comments
- [ ] Example usage in doc tests

---

## Usage Example (Rvue Pattern)

```rust
// UI Thread — create handle
let signal_gc: Gc<SignalDataInner<T>> = create_signal();
let handle: GcHandle<SignalDataInner<T>> = signal_gc.cross_thread_handle();

// Send handle to async thread (handle is Send + Sync, T need not be)
tokio::spawn(async move {
    let result = async_work().await;
    channel.send((handle, result));
});

// UI Thread — resolve and update
for (handle, value) in receiver {
    let signal: Gc<SignalDataInner<T>> = handle.resolve();
    signal.set(value);
    // handle is dropped here, root entry is removed
}
```

### Defensive Pattern (Unknown Thread)

```rust
// When you're not sure which thread you're on:
if let Some(signal) = handle.try_resolve() {
    signal.set(value);
} else {
    // Not on origin thread, or handle was unregistered.
    // Queue the update for the origin thread instead.
    origin_sender.send(UpdateMsg { handle: handle.clone(), value });
}
```

---

## Design Decisions

| Decision | Rationale |
|----------|-----------|
| `gc.cross_thread_handle()` method name | Explicit naming for cross-thread primitives |
| Roots stored on `ThreadControlBlock` (not `LocalRoots`) | **Critical:** enables safe `Drop` from any thread. TCB is `Arc`-shared; `LocalRoots` requires thread-local storage. |
| `Arc<ThreadControlBlock>` in handle | Prevents TCB deallocation after origin thread exits. Enables foreign-thread Drop. |
| `HandleId` + `HashMap` for root entries | O(1) insert/remove. `Vec` would be O(n) removal on drop — unacceptable for handle-heavy workloads. |
| Raw `NonNull<GcBox<T>>` (not weak ref) for strong handle | Root registration guarantees liveness. Weak ref adds indirection with no benefit for strong handles. |
| Panic on wrong thread for `resolve()` | Fail-fast for incorrect usage; matches Rust idioms |
| `try_resolve()` variant | Graceful alternative for contexts where thread identity is uncertain |
| `WeakCrossThreadHandle` also enforces origin-thread affinity | **Safety:** resolving produces `Gc<T>` which must not exist on foreign thread when `T: !Send` |
| Strong handle semantics by default | Matches Rvue's primary use case |
| Weak handle variant included | Future-proofing for "schedule if alive" patterns |
| Handle unregistration is idempotent | Avoids double-free footgun; Drop after unregister is a no-op |
| `Clone` registers independent root | Each clone is a first-class root; no reference counting on root entries |
| `T: 'static` bound required | Handle may outlive the creating scope; prevents dangling lifetime refs in T |
| Thread exit → resolve panics, object stays alive | Simpler than handle migration. Object floats until handles drop. No UB. |
| No barrier in `resolve()` | Barriers belong at stores (GcCell::borrow_mut), not at reads. Existing SATB + Dijkstra barriers handle incremental marking correctly. |

---

## Lock Ordering

The existing lock ordering discipline is extended:

```
LocalHeap → GlobalMarkState → GcRequest → CrossThreadRootTable
```

`CrossThreadRootTable` (the per-TCB mutex) is acquired last. During GC root
scanning, the collector already holds higher-level locks before iterating
TCBs, so acquiring `cross_thread_roots` is safe and deadlock-free.

Handle creation and drop only acquire `cross_thread_roots` — they do not
hold any higher-level GC locks, so there is no risk of inversion.

---

## Performance Considerations

| Operation | Cost | Notes |
|-----------|------|-------|
| `cross_thread_handle()` | Mutex lock + HashMap insert | Cold path; acceptable |
| `resolve()` | ThreadId comparison + pointer deref | Hot path; no lock, no allocation |
| `try_resolve()` | ThreadId comparison + pointer deref | Hot path; no lock, no allocation |
| `clone()` | Mutex lock + HashMap insert + Arc clone | Cold path; acceptable |
| `drop()` | Mutex lock + HashMap remove | Cold path; acceptable |
| GC root scan | Mutex lock + HashMap iteration | Once per GC cycle per thread; O(n) in handle count |

The hot path (`resolve`/`try_resolve`) is lock-free. All lock-taking
operations are on cold paths (creation, clone, drop, GC).

---

## Dependencies & Compatibility

- **No new external dependencies** — uses existing `ThreadId`, `ThreadControlBlock`, `Arc`, `Mutex`, `HashMap`
- **Compatible with existing features**: 008 (incremental marking — see §6), 009 (tracing), 011 (concurrent GC primitives)
- **Feature flag**: No new feature flag needed; part of core API

---

## Testing Strategy

| Test | Description |
|------|-------------|
| `test_cross_thread_send` | Handle sent between threads via channel |
| `test_resolve_origin_thread` | Verify `resolve()` panics on wrong thread |
| `test_try_resolve_wrong_thread` | Verify `try_resolve()` returns `None` on wrong thread |
| `test_handle_keeps_alive` | Verify object not collected while handle exists |
| `test_weak_handle_no_prevent` | Verify weak handle doesn't prevent collection |
| `test_is_valid_checks` | Verify `is_valid()` accuracy |
| `test_clone_independent_lifetime` | Clone keeps object alive independently |
| `test_unregister_idempotent` | Double unregister doesn't panic |
| `test_drop_from_foreign_thread` | Handle dropped on non-origin thread |
| `test_multiple_handles` | Multiple handles to same object |
| `test_origin_thread_exit` | Behavior when origin thread exits |
| `test_downgrade` | Strong-to-weak downgrade preserves semantics |
| `test_miri_thread_safety` | Miri verification for unsafe code |

---

## Deferred Items (Future Features)

1. **Handle migration**: Allow migrating handles to different threads (thread pools)
2. **Handle registry**: Global registry for handles that survive thread exit with named lookup
3. **`AsyncHandle` integration**: Allow `GcHandle` to be used with `AsyncHandleScope`
4. **Batch resolution**: `resolve_many(&[GcHandle<T>]) -> Vec<Gc<T>>` for bulk operations

---

## References

- Feature Request: `rudo-gc-feature-request-cross-thread-handle.md`
- Existing Infrastructure: `AsyncHandleScope`, `GcRootSet`, `LocalHeap`, `ThreadControlBlock`
- Thread Model: `ThreadControlBlock`, `ThreadRegistry`
- Lock Ordering: See `heap.rs` documentation and Feature 001

---

## Bug Fix Notes (2026-02-10)

### Critical Bug Found and Fixed

During code review, a critical bug was discovered: the `iterate_cross_thread_roots` function was defined but **never called during garbage collection**. This meant:

1. Objects referenced by `GcHandle<T>` were not scanned as GC roots
2. Objects could be prematurely collected even while strong cross-thread handles existed
3. This violated FR-003: strong handles MUST keep objects alive

### Root Cause

The function `iterate_cross_thread_roots` was implemented correctly on `ThreadControlBlock`, but the GC marking functions never invoked it during the root scanning phase.

### Fix Applied

**File: `crates/rudo-gc/src/gc/gc.rs`**

1. **`mark_major_roots_multi` (line ~1341)**: Added call to `iterate_cross_thread_roots` after `iterate_all_handles`:
```rust
for (_, tcb) in stack_roots {
    tcb.iterate_cross_thread_roots(|ptr| unsafe {
        if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr.cast::<u8>()) {
            mark_object(gc_box, &mut visitor);
        }
    });
}
```

2. **`mark_major_roots` (line ~1843)**: Added cross-thread root scanning for single-threaded major GC:
```rust
if let Ok(registry) = crate::heap::thread_registry().lock() {
    for tcb in &registry.threads {
        tcb.iterate_cross_thread_roots(|ptr| unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr.cast::<u8>()) {
                mark_object(gc_box, &mut visitor);
            }
        });
    }
}
```

3. **`mark_minor_roots` (line ~1789)**: Added cross-thread root scanning for minor GC to handle young objects referenced by handles:
```rust
if let Ok(registry) = crate::heap::thread_registry().lock() {
    for tcb in &registry.threads {
        tcb.iterate_cross_thread_roots(|ptr| unsafe {
            if let Some(gc_box_ptr) =
                crate::heap::find_gc_box_from_ptr(heap, ptr.cast::<u8>())
            {
                mark_object_minor(gc_box_ptr, &mut visitor);
            }
        });
    }
}
```

**File: `crates/rudo-gc/src/ptr.rs`**

4. **`GcBox::as_weak` (line ~301)**: Fixed to increment `weak_count` when creating weak references:
```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        (*NonNull::from(self).as_ptr()).inc_weak();
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

**File: `crates/rudo-gc/src/heap.rs`**

5. **`iterate_cross_thread_roots` (line ~267)**: Removed `#[allow(dead_code)]` since the function is now used.

**File: `crates/rudo-gc/tests/cross_thread_handle.rs`**

6. Added integration test `test_cross_thread_handle_survives_major_gc` that verifies objects survive major GC when referenced only by cross-thread handles.

### Verification

- All 17 cross-thread handle tests pass
- Full test suite passes
- Clippy passes without warnings
