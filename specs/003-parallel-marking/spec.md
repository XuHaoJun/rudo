# Feature Specification: Parallel Marking for rudo-gc

**Feature Branch**: `003-parallel-marking`  
**Created**: 2026-01-27  
**Status**: Draft  
**Input**: User description: "parallel marking you can read @docs/parallel-marking-spec-1.md and @learn-projects/ChezScheme/"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Multi-threaded GC Performance Improvement (Priority: P1)

As a rudo-gc user running a multi-threaded application with significant heap allocations, I want garbage collection marking to use multiple CPU cores in parallel, so that GC pause times are reduced and application responsiveness improves.

**Why this priority**: Parallel marking is the primary performance improvement for multi-threaded applications. Without it, all marking work happens on a single thread, causing longer pause times as heap size grows.

**Independent Test**: Can be tested by running a multi-threaded application with rudo-gc, forcing garbage collections, and measuring marking phase duration with different numbers of worker threads.

**Acceptance Scenarios**:

1. **Given** a multi-threaded application with 100,000+ reachable Gc objects, **When** a Major GC is triggered, **Then** marking phase completes in time proportional to (work / workers), not work alone.
2. **Given** 4+ CPU cores available, **When** running parallel marking with 4 workers, **Then** marking time should be 50-65% of single-threaded time.
3. **Given** 8+ CPU cores available, **When** running parallel marking with 8 workers, **Then** marking time should be 35-50% of single-threaded time.

---

### User Story 2 - Minor GC with Parallel Marking (Priority: P1)

As a rudo-gc user with generational GC enabled, I want Minor GC to also use parallel marking for old->young references tracked via dirty bits, so that Minor collection pauses remain short even as the old generation grows.

**Why this priority**: Minor GC frequency is typically higher than Major GC. If Minor GC marking is not parallelized, it becomes a bottleneck for applications with frequent young generation collections.

**Independent Test**: Can be tested by creating objects in the old generation that reference young objects, repeatedly allocating and dropping young objects, and measuring Minor GC marking time.

**Acceptance Scenarios**:

1. **Given** an application with 50% old gen and 50% young gen allocations, **When** Minor GC is triggered, **Then** marking should process dirty pages in parallel.
2. **Given** multiple threads each with their own LocalHeap, **When** Minor GC runs, **Then** each thread's dirty pages are processed by appropriate workers.

---

### User Story 3 - Work Stealing Load Balancing (Priority: P2)

As a rudo-gc user with uneven work distribution across threads, I want the parallel marking system to automatically balance work across available workers, so that no worker is idle while work remains.

**Why this priority**: Work stealing ensures that even with skewed allocation patterns, all workers contribute to marking and the fastest workers help complete the work.

**Independent Test**: Can be tested by creating allocation patterns where one thread allocates 10x more objects than others, and verifying that all work is completed and no thread is significantly slower than others.

**Acceptance Scenarios**:

1. **Given** one thread allocates 10,000 objects and other threads allocate 100 each, **When** parallel marking runs, **Then** total marking time should be dominated by the largest queue, not the sum of all queues.
2. **Given** multiple workers with non-empty work queues, **When** a worker finishes its local queue, **Then** it should successfully steal work from other queues.

---

### User Story 4 - Cross-Thread Object References (Priority: P2)

As a rudo-gc user sharing Gc objects between threads, I want all reachable objects to be marked correctly regardless of which thread's heap they reside in, so that object liveness is determined correctly even with complex cross-thread references.

**Why this priority**: Users expect correct GC behavior regardless of reference patterns. Incorrect marking due to threading issues would cause use-after-free bugs.

**Independent Test**: Can be tested by creating a graph of objects distributed across multiple threads' heaps with cross-references, triggering GC, and verifying all reachable objects are retained while unreachable ones are collected.

**Acceptance Scenarios**:

1. **Given** thread A's heap contains an object that references an object in thread B's heap, **When** GC runs, **Then** both objects should be marked as reachable.
2. **Given** an object chain spans three threads (A -> B -> C), **When** GC runs, **Then** the entire chain should be marked and retained.

---

### User Story 5 - Deterministic Termination (Priority: P2)

As a rudo-gc developer, I want the parallel marking algorithm to guarantee that all workers eventually terminate, so that the system cannot deadlock or livelock during garbage collection.

**Why this priority**: GC must complete in finite time. Any possibility of non-termination would make the system unreliable.

**Independent Test**: Can be tested by running GC repeatedly under various workloads and verifying that each collection completes within a reasonable time bound.

**Acceptance Scenarios**:

1. **Given** a valid heap with finite objects, **When** parallel marking starts with N workers, **Then** all workers should complete within time bounded by (total_objects * trace_time_per_object).
2. **Given** workers have completed marking, **When** barrier synchronization runs, **Then** all workers should proceed past the barrier together.

---

### Edge Cases

- What happens when there is only 1 CPU core available? System should fall back to single-threaded marking without overhead.
- How does the system handle a heap with zero reachable objects? All workers should complete immediately.
- What happens when a worker encounters a corrupted or invalid pointer during tracing? The system should skip invalid pointers without crashing.
- How does the system behave when mark bitmaps are already set from a previous collection? `try_mark()` should correctly detect already-marked objects.

---

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST support parallel marking using multiple worker threads, with configurable worker count up to 16.
- **FR-002**: The system MUST use lock-free Chase-Lev work-stealing deques for per-thread work queues.
- **FR-003**: The system MUST provide atomic CAS-based `try_mark()` operation on mark bitmaps to prevent duplicate marking.
- **FR-004**: The system MUST implement page ownership tracking, assigning each heap page to a specific worker for locality.
- **FR-005**: The system MUST support both Major GC parallel marking (all generations) and Minor GC parallel marking (young gen + dirty pages).
- **FR-006**: The system MUST distribute stack roots to appropriate workers based on object page ownership.
- **FR-007**: The system MUST use barrier synchronization to ensure all workers start and finish marking together.
- **FR-008**: The system MUST fall back to single-threaded marking when fewer than 2 workers are available.
- **FR-009**: The system MUST correctly handle cross-thread object references by routing discovered references to the owning worker's queue.

### Key Entities

- **PerThreadMarkQueue**: Per-thread work queue containing local push/pop queue and stealable queue. Handles owned pages and marked object counting.
- **StealQueue<T, N>**: Lock-free ring buffer implementing Chase-Lev deque with LIFO push/pop and FIFO steal operations.
- **ParallelMarkCoordinator**: Orchestrator that manages worker creation, page ownership registration, root distribution, and barrier synchronization.
- **MarkWorker**: Worker thread that processes owned pages and local work queue, then steals from other queues when local work is exhausted.
- **GcVisitorConcurrent**: Visitor implementation that routes discovered object references to appropriate workers' queues.

---

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Parallel marking with 4 workers MUST complete marking phase in 35-45% of single-threaded time for typical workloads with balanced allocation.
- **SC-002**: Parallel marking with 8 workers MUST complete marking phase in 25-35% of single-threaded time.
- **SC-003**: All reachable objects MUST be correctly marked in parallel marking mode, with no objects incorrectly swept as garbage (100% marking completeness).
- **SC-004**: No objects SHOULD be marked more than once (verified by tracking `try_mark()` return values).
- **SC-005**: All workers MUST complete within bounded time proportional to total work, with no indefinite blocking or livelock.
- **SC-006**: GC pause time reduction SHOULD be proportional to number of available workers, up to 16 workers maximum.

### Business Outcomes

- **SC-007**: Multi-threaded applications using rudo-gc SHOULD experience reduced GC pause times, improving overall application responsiveness.
- **SC-008**: Users SHOULD be able to configure parallel marking worker count to balance performance vs. CPU usage.

---

## Assumptions

1. The existing GC handshake and thread coordination mechanism remains functional and will be reused.
2. Each heap page is allocated by a specific thread and that thread's LocalHeap owns the page.
3. Object references within a page are contiguous and can be processed in bulk.
4. The mark bitmap in PageHeader is already atomic and can be used with CAS operations.
5. Worker count will default to `min(num_cpus, 16)` based on common GC design practices.

---

## Dependencies

- Existing thread-local heap structure (LocalHeap) for page ownership
- Existing GC handshake mechanism (request_gc_handshake, etc.)
- Existing PageHeader with atomic mark_bitmap
- Existing Trace trait and visitor pattern for object traversal

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Race conditions in parallel marking | High -可能导致use-after-free | CAS-based `try_mark()` with proper memory ordering |
| Work stealing synchronization overhead | Medium -影響效能 | Lock-free Chase-Lev algorithm minimizes contention |
| Complex cross-thread reference routing | Medium -可能標記不完全 | HashMap lookup for page-to-queue mapping |
| Memory ordering bugs | High -未定義行為 | Careful use of Acquire/Release ordering per operation |

---

## Out of Scope

- Concurrent marking (marking while mutators run) - this is parallel stop-the-world marking only
- Parallel sweeping (sweep phase parallelization)
- Dynamic worker count adjustment during collection
- NUMA-aware page placement
