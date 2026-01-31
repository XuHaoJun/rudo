# Feature Specification: Implement Lazy Sweep for Garbage Collection

**Feature Branch**: `005-lazy-sweep`
**Created**: 2026-01-31
**Status**: Draft
**Input**: User description: "add lazy sweep @docs/lazy-sweep-plan.md"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Eliminate STW Pause During GC Sweep (Priority: P1)

As a developer using rudo-gc, I want garbage collection sweep operations to happen incrementally during normal allocation instead of stopping the entire application, so that my application experiences consistent and predictable performance without long pauses.

**Why this priority**: This is the core value proposition of lazy sweep. Eliminating stop-the-world (STW) pause times is the primary motivation for this feature. Applications with strict latency requirements (real-time systems, gaming, interactive applications) cannot tolerate unpredictable pauses. This delivers immediate, tangible value to users.

**Independent Test**: Can be tested by running GC-intensive workloads with latency measurements, verifying that maximum pause times remain bounded regardless of heap size or number of objects.

**Acceptance Scenarios**:

1. **Given** a heap with many dead objects after a collection, **When** the application performs new allocations, **Then** sweep work is performed incrementally during those allocations without stopping the application.

2. **Given** a running application under load, **When** a GC collection occurs, **Then** the application does not experience a pause proportional to the number of pages or objects in the heap.

3. **Given** a large heap with millions of objects, **When** collection and allocation happen concurrently, **Then** allocation latency remains consistent (within 2x of normal allocation time) rather than spiking during sweep.

---

### User Story 2 - Memory Reclaimed During Allocation (Priority: P1)

As a developer, I want dead objects to be reclaimed during allocation operations, so that memory is reused efficiently without requiring a separate sweep phase that delays new allocations.

**Why this priority**: This ensures that lazy sweep actually delivers reclaimed memory to the allocator, preventing unbounded heap growth. Without this, the feature would not solve the core memory reclamation problem.

**Independent Test**: Can be tested by creating a workload that allocates and discards many objects, then verifying that heap size stabilizes and reclaimed memory is reused for new allocations.

**Acceptance Scenarios**:

1. **Given** a page with dead objects that have been swept, **When** the application allocates a new object, **Then** the allocation can use memory reclaimed from the swept dead objects.

2. **Given** repeated allocation cycles with objects going out of scope, **When** collections occur, **Then** the process memory footprint remains bounded and does not grow indefinitely.

3. **Given** insufficient free memory in existing pages, **When** allocation triggers lazy sweep, **Then** reclaimed objects are added to free lists and available for new allocations.

---

### User Story 3 - Lazy Sweep Behavior Defaults (Priority: P2)

As a user of rudo-gc, I want lazy sweep to be enabled by default, so that I get better performance characteristics without needing to configure anything or understand internal implementation details.

**Why this priority**: This ensures users get the best experience automatically. The default behavior should match what most users want (eliminated pause times) rather than requiring explicit opt-in.

**Independent Test**: Can be tested by building rudo-gc with default features and verifying lazy sweep is active without any configuration.

**Acceptance Scenarios**:

1. **Given** rudo-gc built with default features, **When** a collection occurs, **Then** lazy sweep is used automatically (no STW sweep phase).

2. **Given** a user who wants eager sweep behavior, **When** they disable the lazy-sweep feature, **Then** the traditional synchronous sweep behavior is used.

---

### User Story 4 - Large Objects Use Eager Sweep (Priority: P2)

As a developer, I want large objects (those larger than a page) to be handled differently from regular objects, so that they are reclaimed promptly in an all-or-nothing fashion without complicating the lazy sweep mechanism.

**Why this priority**: Large objects have different characteristics (size, allocation patterns) that make lazy sweep less beneficial. Keeping them eager simplifies the implementation and ensures predictable cleanup.

**Independent Test**: Can be tested by allocating large objects, discarding them, and verifying they are reclaimed promptly (not waiting for allocation-triggered lazy sweep).

**Acceptance Scenarios**:

1. **Given** a large object allocated on its own page, **When** the object goes out of scope and collection occurs, **Then** the entire page is reclaimed eagerly (not lazily).

2. **Given** mixed workloads with regular and large objects, **When** collection occurs, **Then** large objects are swept eagerly while regular objects use lazy sweep.

---

### User Story 5 - Weak References Handled Correctly (Priority: P2)

As a developer using weak references, I want objects with weak references to be handled correctly during lazy sweep, so that weak references continue to function correctly and dead objects are cleaned up appropriately.

**Why this priority**: Weak references are a critical feature for caches and other patterns. If lazy sweep breaks weak reference semantics, users would experience incorrect behavior (accessing dead objects).

**Independent Test**: Can be tested by creating weak references to objects, discarding strong references, and verifying weak references correctly report dead status after collection and lazy sweep.

**Acceptance Scenarios**:

1. **Given** an object with weak references, **When** the strong reference is dropped and collection occurs, **Then** the weak references correctly report the object as dead after lazy sweep.

2. **Given** an object with weak references where the value has been dropped but allocation remains, **When** lazy sweep processes the page, **Then** the allocation remains until the weak reference is also dropped.

---

### User Story 6 - Public API for Sweep Control (Priority: P3)

As a developer, I want programmatic access to sweep operations, so that I can trigger sweep work explicitly when appropriate for my application's needs.

**Why this priority**: While not critical for basic functionality, having API access to sweep operations enables advanced use cases and testing scenarios.

**Independent Test**: Can be tested by calling the public API functions and verifying they return correct information about pending sweep work.

**Acceptance Scenarios**:

1. **Given** pages pending sweep, **When** the application calls `sweep_pending(num_pages)`, **Then** the function returns the number of pages actually swept.

2. **Given** lazy sweep is enabled, **When** the application calls `pending_sweep_pages()`, **Then** it returns an accurate count of pages awaiting sweep.

---

### Edge Cases

- What happens when a collection occurs during a safepoint check and lazy sweep work is triggered?
- How does the system handle pages where all objects are dead (the "all-dead" optimization)?
- What happens when weak references are dropped after lazy sweep has partially processed a page?
- How does the system behave when allocation rate exceeds lazy sweep rate (can heap grow unboundedly)?
- What happens during concurrent collections with multiple threads performing lazy sweep?

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST perform sweep operations incrementally during allocation rather than synchronously after marking.
- **FR-002**: System MUST eliminate the stop-the-world (STW) sweep phase during garbage collection.
- **FR-003**: System MUST reclaim memory from dead objects and make it available for new allocations.
- **FR-004**: System MUST bound allocation latency to O(1) amortized time per object (excluding actual allocation work).
- **FR-005**: System MUST provide lazy sweep behavior by default when the lazy-sweep feature is enabled.
- **FR-006**: System MUST provide eager sweep behavior when the lazy-sweep feature is disabled.
- **FR-007**: System MUST handle large objects (larger than a page) using eager sweep, not lazy sweep.
- **FR-008**: System MUST handle orphan pages (pages with no live roots) using eager sweep for prompt reclamation.
- **FR-009**: System MUST correctly handle weak references during lazy sweep, preserving allocation until weak ref is dropped.
- **FR-010**: System MUST provide a public API function `sweep_pending(num_pages)` that sweeps up to the specified number of pages.
- **FR-011**: System MUST provide a public API function `pending_sweep_pages()` that returns the count of pages awaiting sweep.
- **FR-012**: System MUST use a batch size of up to 16 objects per page during lazy sweep to bound per-allocation overhead.
- **FR-013**: System MUST track which pages need sweep using per-page flags (PAGE_FLAG_NEEDS_SWEEP and PAGE_FLAG_ALL_DEAD).
- **FR-014**: System MUST periodically perform lazy sweep work during safepoint checks to prevent unbounded heap growth.
- **FR-015**: System MUST process "all dead" pages using a fast path that rebuilds free lists without examining individual objects.

### Key Entities *(include if feature involves data)*

- **PageHeader**: Per-page metadata structure tracking page state, including flags for sweep state and dead object count.
- **Sweep Flags**: Bit flags (PAGE_FLAG_NEEDS_SWEEP, PAGE_FLAG_ALL_DEAD) indicating page sweep status.
- **Dead Object Counter**: Per-page counter tracking number of dead objects to enable "all-dead" optimization.
- **Free List**: Per-page linked list of reclaimed objects available for allocation.
- **Lazy Sweep Batch**: Fixed-size batch (16 objects) of sweep work performed during each lazy sweep operation.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: GC pause times during sweep phase MUST be reduced from O(pages + objects) to O(1) amortized per allocation.
- **SC-002**: Maximum pause time during lazy sweep MUST be bounded regardless of heap size (target: under 100 microseconds per allocation-triggered sweep).
- **SC-003**: Memory reclaimed via lazy sweep MUST be available for new allocations within the same allocation cycle that triggered the sweep.
- **SC-004**: Heap memory growth MUST be bounded for workloads with allocation and deallocation patterns (no unbounded growth).
- **SC-005**: Large objects MUST be reclaimed within one collection cycle (not delayed until lazy sweep during allocation).
- **SC-006**: Weak reference semantics MUST be preserved (weak refs report dead status correctly after lazy sweep).
- **SC-007**: Performance overhead per allocation MUST be minimal when no sweep work is needed (target: less than 5% overhead).
- **SC-008**: Feature MUST be enabled by default, with users able to disable via build configuration.

---

**Assumptions**:

- The lazy-sweep feature will be controlled by a Cargo feature flag `lazy-sweep` that defaults to enabled.
- Eager sweep will remain available as a fallback for testing, benchmarking, and edge cases (large objects, orphans).
- Per-page overhead increase (1 flag byte + 2 bytes for dead count) is acceptable given the pause time benefits.
- Memory reclamation timing is acceptable to be delayed until next allocation (not immediate).
- Slight increase in fragmentation is acceptable trade-off for eliminated pause times.

**Dependencies**:

- Existing mark-sweep collector infrastructure (mark phase, page management, allocation path).
- Existing safepoint mechanism for triggering background work during collection.
- Existing feature flag infrastructure for optional features.
