# Feature Specification: Concurrent GC Primitives

**Feature Branch**: `011-concurrent-gc-primitives`
**Created**: 2026-02-08
**Status**: Implemented (2026-02-08)
**Input**: User description: "Implement thread-safe concurrent GC primitives (GcRwLock and GcMutex) for sharing garbage-collected objects across threads with lock-bypass during STW pauses"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Multi-threaded GC Object Sharing (Priority: P1)

As a developer building a multi-threaded application using rudo-gc, I want to share garbage-collected objects between threads safely, so that I can build concurrent data structures like shared caches, queues, and state machines.

**Why this priority**: This is the core value proposition of the feature - enabling multi-threaded use of GC objects. Without this, users cannot build concurrent applications with rudo-gc.

**Independent Test**: Can be tested by creating a shared GC object protected by GcRwLock or GcMutex, accessing it from multiple threads concurrently, and verifying no data races or memory errors occur.

**Acceptance Scenarios**:

1. **Given** a GcRwLock wrapping a GC-allocated object, **When** multiple reader threads call `read()` concurrently, **Then** all readers access the data safely without blocking each other.

2. **Given** a GcMutex wrapping a GC-allocated object, **When** multiple threads compete for the lock, **Then** only one thread holds the lock at a time and others wait their turn.

3. **Given** a GcRwLock wrapping a GC-allocated object, **When** a writer thread calls `write()` and readers are active, **Then** writers proceed after all readers release their locks, and all waiting readers/writers proceed after the write completes.

---

### User Story 2 - Performance Isolation for Single-Threaded Users (Priority: P1)

As a developer using rudo-gc for single-threaded workloads (DOM trees, ASTs, scripting engines), I want to continue using GcCell without synchronization overhead, so that my performance-critical single-threaded code is not penalized by multi-threading features.

**Why this priority**: Maintains backward compatibility and performance for existing single-threaded users, ensuring they don't pay for features they don't need.

**Independent Test**: Can be tested by comparing performance of GcCell operations in single-threaded scenarios, verifying atomic operation overhead is not introduced.

**Acceptance Scenarios**:

1. **Given** existing code using GcCell, **When** migrated to the new version with GcRwLock/GcMutex, **Then** GcCell remains unchanged and continues to work without modification.

2. **Given** a single-threaded benchmark, **When** using GcCell, **Then** performance matches or exceeds the previous implementation (no regression).

---

### User Story 3 - GC Safety During Mark Phase (Priority: P1)

As the garbage collector runtime, I need to trace all reachable GC objects during the Stop-The-World (STW) mark phase without deadlocking on user locks, so that memory safety is maintained for all concurrent programs.

**Why this priority**: This is critical for correctness - if the GC deadlocks while tracing, the entire application hangs.

**Independent Test**: Can be tested by creating concurrent GC operations under heavy lock contention and verifying the GC completes without deadlock regardless of lock state.

**Acceptance Scenarios**:

1. **Given** multiple threads holding locks on GcRwLock/GcMutex, **When** GC STW pause begins, **Then** all mutator threads are suspended and the GC traces objects by bypassing locks.

2. **Given** a thread paused mid-write during STW, **When** GC traces that object, **Then** the trace reads a consistent, atomic pointer value (no torn reads).

---

### User Story 4 - Write Barrier Integration (Priority: P2)

As a developer using incremental/major GC features, I want write barriers to be triggered automatically when I modify protected GC objects, so that the GC can correctly track object relationships for generational and SATB marking.

**Why this priority**: Required for incremental marking features to work correctly with concurrent primitives. Without proper barriers, the GC may miss references.

**Independent Test**: Can be tested by modifying GC objects through GcRwLock/GcMutex guards and verifying dirty pages/lists are updated correctly.

**Acceptance Scenarios**:

1. **Given** a GcRwLock write guard, **When** modifying the protected object's fields, **Then** generational barriers mark the page as dirty.

2. **Given** incremental GC configuration, **When** writing to a GcRwLock/GcMutex protected object, **Then** SATB barriers record the old value for snapshot consistency.

---

### Edge Cases

- What happens when a thread holding a GcRwLock/GcMutex panics? (Guard must be dropped, lock must be released)
- How does the system handle nested GC tracing through concurrent primitives?
- What happens with empty concurrent primitives (zero-sized locks)?
- How does lock poisoning work if a panic occurs while holding the lock?

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST provide a `GcRwLock<T>` type that implements `Send + Sync` when `T: Trace + Send + Sync`.

- **FR-002**: The system MUST provide a `GcMutex<T>` type that implements `Send + Sync` when `T: Trace + Send`.

- **FR-003**: The system MUST retain the existing `GcCell<T>` type with `!Sync` semantics for single-threaded use cases.

- **FR-004**: The `Trace` implementation for `GcRwLock<T>` and `GcMutex<T>` MUST bypass the lock mechanism to avoid deadlock during GC STW pauses.

- **FR-005**: The system MUST trigger write barriers (generational and SATB) when acquiring mutable guards (`write()` for GcRwLock, `lock()` for GcMutex).

- **FR-006**: The system MUST allow multiple concurrent readers through `GcRwLock::read()` without blocking.

- **FR-007**: The system MUST provide exclusive access through `GcRwLock::write()` and `GcMutex::lock()`.

- **FR-008**: The system MUST ensure all memory operations on GC pointers within concurrent primitives are safe for concurrent access by the GC during STW.

### Key Entities *(include if feature involves data)*

- **GcRwLock<T>**: Reader-writer lock wrapper for GC objects, optimized for read-heavy concurrent workloads.

- **GcMutex<T>**: Exclusive mutex wrapper for GC objects, optimized for write-heavy concurrent workloads.

- **GcCell<T>**: Existing single-threaded interior mutability primitive. Remains unchanged.

- **GcRwLockReadGuard**: RAII guard returned by `GcRwLock::read()`. Provides shared read access.

- **GcRwLockWriteGuard**: RAII guard returned by `GcRwLock::write()`. Provides exclusive write access with write barrier triggering.

- **GcMutexGuard**: RAII guard returned by `GcMutex::lock()`. Provides exclusive access with write barrier triggering.

- **Write Barrier**: Mechanism to inform GC of object modifications. Includes generational barrier (dirty page tracking) and SATB barrier (old value capture).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Multi-threaded applications CAN share GC objects safely, with zero data races detected by Miri or ThreadSanitizer in concurrent access scenarios.

- **SC-002**: GC tracing during STW pauses COMPLETES without deadlock, even when all threads hold concurrent locks (verified by stress tests with maximum contention).

- **SC-003**: Single-threaded performance with GcCell REMAINS unchanged, with no measurable regression in benchmarks (within noise margin).

- **SC-004**: Concurrent primitive overhead is limited to atomic synchronization, with read throughput through GcRwLock matching or exceeding custom reader-writer implementations for GC workloads.

- **SC-005**: Write barrier triggering adds negligible overhead (microseconds scale) compared to lock acquisition time.

---

## Assumptions

- The `parking_lot` crate is available and can be used for lock implementations (standard in high-performance Rust code).

- The existing `Trace` trait infrastructure supports implementing Trace for concurrent primitives.

- Write barrier infrastructure from GcCell can be reused or adapted for concurrent primitives.

- The GC's STW pause mechanism can safely read raw pointers without synchronization.

- Users will use GcCell for single-threaded code and GcRwLock/GcMutex for multi-threaded code (documented in migration guide).

## Dependencies

- `parking_lot` crate for efficient lock implementations.

- Existing `Trace` trait and `Visitor` infrastructure.

- Existing write barrier implementation in GcCell.

- Existing STW pause mechanism in GC runtime.

## Out of Scope

- Custom lock implementations (using parking_lot as standard).

- Lock timeout APIs (standard parking_lot behavior: blocks indefinitely).

- Fairness policies (parking_lot's default fairness).

- Async-aware locks (separate future feature).

## Implementation Notes (2026-02-08)

### Write Barrier Integration

Write barriers are triggered on guard acquisition (not during field mutations):

```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T> {
    // Barrier triggered here during lock acquisition
    self.trigger_write_barrier();
    let guard = self.inner.write();
    GcRwLockWriteGuard { guard, _marker: PhantomData }
}

pub fn lock(&self) -> GcMutexGuard<'_, T> {
    // Barrier triggered here during lock acquisition
    self.trigger_write_barrier();
    let guard = self.inner.lock();
    GcMutexGuard { guard, _marker: PhantomData }
}
```

### Barrier Types

- **Generational Barrier**: Marks old-generation pages dirty when GcRwLock/GcMutex is modified
- **SATB Barrier**: Records pages in remembered buffer during incremental marking

### Trait Bounds

Send/Sync bounds require `T: Trace + Send + Sync` (including `?Sized` types):

```rust
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcRwLock<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcRwLock<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcMutex<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcMutex<T> {}
```

### Lock Bypass During GC

The Trace implementation bypasses locks during STW pauses:

```rust
unsafe impl<T: Trace + ?Sized> Trace for GcRwLock<T> {
    fn trace(&self, visitor: &mut impl Visitor) {
        let raw_ptr = self.inner.data_ptr();
        unsafe { (*raw_ptr).trace(visitor); }
    }
}
```

### Panic Recovery

Both GcRwLock and GcMutex recover gracefully after thread panics. Parking_lot locks do not poison on panic, allowing subsequent access to succeed.
