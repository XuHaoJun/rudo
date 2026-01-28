# Feature Specification: Chez Scheme GC optimizations for rudo-gc

**Feature Branch**: `001-chez-gc-optimization`
**Created**: 2026-01-27
**Status**: Draft
**Input**: User description: "optimize! read @docs/chez-optimization-plan-1.md "

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Reduced GC pause times in multi-threaded applications (Priority: P1)

Application developers using rudo-gc in multi-threaded programs experience shorter garbage collection pauses, improving overall application responsiveness and throughput.

**Why this priority**: GC pauses directly impact application latency and user experience. Multi-threaded applications are the primary use case for parallel marking, so reducing pause times delivers immediate value to the largest user base.

**Independent Test**: Can be tested by running concurrent allocation workloads across multiple threads and measuring pause time distribution. Success is demonstrated when 95th percentile pause times are within acceptable thresholds.

**Acceptance Scenarios**:

1. **Given** an application with 4+ threads performing allocations, **When** a garbage collection cycle runs, **Then** the pause time should be distributed across worker threads rather than serialized.

2. **Given** high contention scenarios with many workers attempting to steal work simultaneously, **When** workers become idle, **Then** they should efficiently receive work from other workers without excessive contention.

3. **Given** a mix of local and remote page allocations, **When** marking phase executes, **Then** workers should prioritize marking pages they own to maximize cache locality.

---

### User Story 2 - Prevention of deadlock and race conditions (Priority: P1)

Developers can rely on rudo-gc to correctly handle concurrent access without introducing deadlocks or race conditions, even as the codebase evolves with new features.

**Why this priority**: Deadlocks cause application hangs and are extremely difficult to diagnose. Prevention through systematic lock ordering is foundational for reliability.

**Independent Test**: Can be tested by running concurrent GC cycles under heavy load while introducing new lock acquisitions. Success is demonstrated when no deadlocks occur across extended test runs with randomized lock acquisition patterns.

**Acceptance Scenarios**:

1. **Given** multiple threads holding different locks, **When** a thread needs to acquire a lock held by another thread, **Then** the lock acquisition order follows documented discipline without exceptions.

2. **Given** debug builds with lock ordering validation enabled, **When** lock ordering violations occur, **Then** the system reports violations immediately rather than silently proceeding.

3. **Given** changes to lock usage in future development, **When** new lock acquisition patterns are introduced, **Then** the lock ordering discipline is documented and enforced.

---

### User Story 3 - Memory efficiency in long-running applications (Priority: P2)

Applications using rudo-gc for extended periods consume less memory overhead per allocated object, allowing more objects to fit in the heap before collection is needed.

**Why this priority**: Memory efficiency directly impacts application scalability and reduces GC frequency. This is particularly important for long-running services where memory pressure accumulates over time.

**Independent Test**: Can be tested by allocating many small objects and measuring per-object overhead. Success is demonstrated when memory overhead per object is minimized compared to baseline.

**Acceptance Scenarios**:

1. **Given** applications with many small objects (under 64 bytes), **When** objects are allocated and marked, **Then** the marking metadata occupies minimal space relative to object data.

2. **Given** a heap with mixed object sizes, **When** the mark phase completes, **Then** the bitmap accurately records object liveness without per-object forwarding pointers.

3. **Given** backward compatibility requirements, **When** existing code relies on forwarding pointers, **Then** the system continues to function correctly while new code can use the more efficient bitmap approach.

---

### User Story 4 - Predictable performance under varying workloads (Priority: P2)

Developers can predict rudo-gc behavior across different workload patterns, including burst allocations, steady-state operation, and varying thread counts.

**Why this priority**: Predictable performance enables capacity planning and helps developers understand when to tune GC parameters versus when to reconsider allocation patterns.

**Independent Test**: Can be tested by running standardized benchmark workloads with varying allocation rates and thread counts. Success is demonstrated when throughput and pause times remain within predictable bounds.

**Acceptance Scenarios**:

1. **Given** a burst of allocations that fills the work queue, **When** workers process marking work, **Then** the queue dynamically grows or distributes work to prevent stalls.

2. **Given** varying numbers of worker threads (2, 4, 8, 16), **When** GC cycles execute, **Then** performance scales reasonably with additional workers without regression.

3. **Given** workloads with different object graph shapes, **When** the mark phase runs, **Then** work distribution remains balanced across workers regardless of reference patterns.

---

### Edge Cases

- What happens when all workers simultaneously become idle with no work available?
- How does the system handle heterogeneous page sizes with different ownership models?
- What occurs during GC when some workers are slow or unresponsive?
- How are lock ordering violations handled in production (non-debug) builds?
- What is the behavior when the mark bitmap capacity is exceeded?

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST reduce contention during work stealing by implementing push-based work transfer, where workers notify other workers of available work rather than having all workers poll all queues.

- **FR-002**: System MUST integrate segment/page ownership information into work distribution, allowing workers to prioritize marking pages they own for better cache locality.

- **FR-003**: System MUST implement mark bitmap for in-place marking that records object liveness using one bit per pointer-sized unit, reducing per-object overhead compared to forwarding pointers.

- **FR-004**: System MUST define and enforce lock ordering discipline across all mutex acquisition points, preventing potential deadlocks regardless of acquisition order in new code.

- **FR-005**: System MUST provide dynamic stack growth monitoring to handle queue capacity under varying allocation patterns, preventing stalls when queues become full.

- **FR-006**: System MUST migrate from forwarding pointer marking to bitmap-based marking, with a one-time update to existing code that relies on forwarding pointers.

### Key Entities

- **PerThreadMarkQueue**: A thread-local work queue that holds marking work items to be processed. Tracks both local queue and pending work received from other threads.

- **PageHeader**: Metadata structure for each heap page that includes ownership information (which thread created the page) and optional marking bitmap.

- **GlobalMarkState**: Coordinator that manages all worker queues and tracks overall mark phase progress. Implements work distribution policies.

- **MarkBitmap**: Page-level structure that records which objects have been marked, using one bit per pointer-sized unit instead of per-object forwarding pointers.

- **LockOrderingDiscipline**: Documented and enforced rules specifying the valid order in which locks may be acquired, preventing circular wait conditions.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 95th percentile GC pause time is reduced by at least 30% compared to baseline, measured under representative multi-threaded workloads.

- **SC-002**: Work stealing contention is reduced such that successful steals complete without repeated retry attempts in 90% of cases under high-load conditions.

- **SC-003**: Per-object memory overhead is reduced by at least 50% for small objects (under 64 bytes) when using bitmap marking compared to forwarding pointer approach.

- **SC-004**: No deadlocks occur during extended concurrent GC testing (minimum 24 hours of continuous operation with randomized workloads).

- **SC-005**: Lock ordering violations are detected and reported in debug builds, with zero violations remaining in the codebase.

- **SC-006**: Performance scales proportionally with worker count up to 16 threads, with no regression in throughput when adding workers.

## Assumptions

- The rudo-gc codebase already has Chase-Lev work-stealing deque and parallel marking coordinator implemented; optimizations build upon this foundation.

- Applications using rudo-gc are primarily multi-threaded services where GC pauses impact latency.

- Memory overhead from forwarding pointers is a measurable concern for workloads with many small objects.

- Lock ordering violations are currently possible but have been addressed ad-hoc; systematic discipline is needed for maintainability.

- The optimization targets Rust 1.75+ as specified in AGENTS.md for the project.

## Dependencies

- Existing parallel marking implementation in `marker.rs` and work-stealing deque in `worklist.rs`.

- PageHeader structure already exists with `owner_thread` field that needs further integration.

- Build system includes clippy, format, and test scripts that will validate changes.

## Out of Scope

- Card-based dirty tracking for minor GC optimization (marked as Phase 3 future consideration).

- Hybrid copy/mark-sweep for reduced fragmentation (marked as Phase 3 future consideration).

- Ephemeron support for weak references (marked as Phase 3 future consideration).

- Changes to allocation algorithms themselves (focus is on mark phase optimization).

## Open Questions (Resolved)

1. **FR-003 Implementation**: Mark bitmap will completely replace forwarding pointers with a one-time migration. Existing code will be updated to use the new bitmap-based approach.

2. **FR-001 Buffer Sizing**: Push-based work queue will use a fixed small buffer (8-16 items) to minimize memory overhead while maintaining efficient work transfer.

3. **FR-002 Page Sizes**: System will require uniform page sizes for ownership-based load distribution, simplifying implementation and maintaining clear contracts with users.
