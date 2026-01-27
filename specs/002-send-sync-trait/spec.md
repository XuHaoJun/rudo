# Feature Specification: Send + Sync Trait Support

**Feature Branch**: `002-send-sync-trait`
**Created**: 2026-01-27
**Status**: Draft
**Input**: User description: "support Send + Sync trait more detail at @docs/send-sync-spec.md"

---

## Clarifications

### Session 2026-01-27

- Q: Feature Scope Boundary → A: Parallel marking is explicitly OUT OF SCOPE - only Send/Sync trait bounds, not concurrent collection
- Q: Reference Count Overflow Handling → A: Saturating counter - clamp at `isize::MAX` and ignore further increments
- Q: Atomic Operation Failure Handling → A: CAS loop with exponential backoff on contention
- Q: Concurrent Collection Behavior → A: Generation-based - collection operates on young gen separately, major GC uses safepoint protocol
- Q: Throughput Expectations → A: No specific throughput target - correctness is sufficient

---

## Out of Scope

- Parallel marking (concurrent GC collection across threads)
- Work-stealing during collection phases
- Concurrent scanning of heap objects during collection

---

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Multi-threaded Gc Pointer Sharing (Priority: P1)

**As a** Rust developer building multi-threaded applications,
**I want** to share garbage-collected pointers (`Gc<T>`) across threads,
**So that** I can build concurrent data structures without manual synchronization of reference counting.

**Why this priority**: This is the core capability that enables rudo-gc to be used in multi-threaded applications, which is a fundamental limitation currently.

**Independent Test**: Can be verified by creating a `Gc<T>` on one thread, cloning it, sending to another thread, and successfully dereferencing on that thread.

**Acceptance Scenarios**:

1. **Given** a `Gc<T>` where `T: Send + Sync`, **When** the pointer is cloned and sent to another thread, **Then** both threads can safely dereference the `Gc<T>`.
2. **Given** a `Gc<T>` shared across threads via `Arc`, **When** both threads perform concurrent clone and drop operations, **Then** reference count remains consistent and no memory corruption occurs.

---

### User Story 2 - Thread-safe Weak References (Priority: P2)

**As a** Rust developer using weak references to break cycles,
**I want** `Weak<T>` pointers to be thread-safe,
**So that** I can use them in multi-threaded contexts without data races.

**Why this priority**: Weak references are essential for handling cyclic data structures, and their thread safety is required for full multi-threaded support.

**Independent Test**: Can be verified by creating `Weak<T>` references across threads, upgrading them on different threads, and ensuring the upgrade succeeds only when the strong reference exists.

**Acceptance Scenarios**:

1. **Given** a `Weak<T>` where `T: Send + Sync`, **When** the weak reference is sent to another thread, **Then** `upgrade()` can be called safely on that thread.
2. **Given** a `Weak<T>` referencing a dropped value, **When** `upgrade()` is called from any thread, **Then** it returns `None` without panicking.

---

### User Story 3 - Concurrent GC Operations (Priority: P3)

**As a** developer running rudo-gc in a multi-threaded environment,
**I want** garbage collection to work correctly when pointers are shared across threads,
**So that** memory is properly reclaimed even with concurrent access patterns.

**Why this priority**: Correctness of GC during concurrent access is critical for preventing memory leaks and use-after-free bugs.

**Independent Test**: Can be verified by running GC collection while multiple threads hold and modify `Gc<T>` pointers, ensuring no crashes or memory corruption.

**Acceptance Scenarios**:

1. **Given** multiple threads holding `Gc<T>` pointers, **When** GC collection is triggered, **Then** all reachable objects are preserved and unreachable objects are reclaimed.
2. **Given** a thread performing `clone()` during collection, **When** the operation completes, **Then** reference counts are accurate.

---

### Edge Cases

- **Reference count overflow**: Reference count saturates at `isize::MAX` - further increments are ignored
- **Atomic operation failure**: Compare-And-Swap operations use CAS loop with exponential backoff on contention
- **Memory ordering violations**: Incorrect ordering may cause stale reads; proper ordering enforced by API design
- **Concurrent collection and allocation**: Generation-based collection operates on young gen separately; major GC uses existing safepoint protocol to coordinate threads

---

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: `Gc<T>` MUST implement `Send` when `T: Trace + Send + Sync`
- **FR-002**: `Gc<T>` MUST implement `Sync` when `T: Trace + Send + Sync`
- **FR-003**: `Weak<T>` MUST implement `Send` when `T: Trace + Send + Sync`
- **FR-004**: `Weak<T>` MUST implement `Sync` when `T: Trace + Send + Sync`
- **FR-005**: All reference count operations MUST use atomic operations (`AtomicUsize`)
- **FR-006**: All pointer load operations MUST use `Acquire` ordering
- **FR-007**: All pointer store operations MUST use `Release` ordering
- **FR-008**: Reference decrement MUST use `AcqRel` ordering to ensure proper synchronization
- **FR-009**: All unsafe code MUST have explicit SAFETY comments explaining memory safety guarantees
- **FR-010**: Miri tests MUST pass for all atomic reference count operations

### Key Entities

- **GcBox**: Internal heap allocation containing reference count, weak count, and user value
  - `ref_count: AtomicUsize` - Strong reference count (atomic)
  - `weak_count: AtomicUsize` - Weak reference count (atomic)
  - `value: T` - User data

- **Gc<T>**: Smart pointer providing shared ownership
  - `ptr: AtomicPtr<GcBox<T>>` - Atomic pointer to GcBox

- **Weak<T>**: Weak reference that does not prevent collection
  - `ptr: AtomicPtr<GcBox<T>>` - Atomic pointer to GcBox

---

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: `Gc<Arc<AtomicUsize>>` passes `assert_send_and_sync` compile-time check
- **SC-002**: Multi-threaded clone/drop stress test completes with zero data races detected by ThreadSanitizer
- **SC-003**: All Miri tests pass for atomic reference count operations
- **SC-004**: Reference count operations complete within expected timing bounds (clone < 100ns, drop < 150ns)
- **SC-005**: Memory safety verified: zero use-after-free, double-free, or data race reports in CI
- **SC-006**: API remains backward compatible for single-threaded usage (existing tests pass)

---

## Assumptions

- Rust's `std::sync::atomic` module handles platform-specific memory ordering differences correctly
- The existing multi-threaded GC infrastructure (ThreadRegistry, ThreadControlBlock) is compatible with Send/Sync Gc<T>
- Performance overhead of atomic operations is acceptable for multi-threaded use cases
- The project constitution's Memory Safety principle applies: all unsafe code must have SAFETY comments
