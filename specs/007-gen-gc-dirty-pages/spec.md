# Feature Specification: Generational GC Dirty Page Tracking

**Feature Branch**: `007-gen-gc-dirty-pages`  
**Created**: 2026-02-03  
**Status**: Draft  
**Input**: Optimize minor collection pause times by implementing dirty page tracking for the generational garbage collector.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Reduced Minor GC Pause Times (Priority: P1)

As a developer using rudo-gc in a latency-sensitive application, I want minor GC pauses to be significantly shorter so that my application maintains consistent responsiveness even with large heaps.

**Why this priority**: This is the primary goal of the feature. Minor GC pause times directly impact application latency and user experience. The current implementation iterates O(num_pages) to find dirty objects, causing unnecessary overhead when only a small subset of pages contain dirty objects.

**Independent Test**: Can be fully tested by running a benchmark that measures minor GC pause times before and after the optimization, demonstrating measurable improvement with large heaps containing few dirty pages.

**Acceptance Scenarios**:

1. **Given** a heap with 1000 old-generation pages and only 10 dirty pages, **When** a minor GC is triggered, **Then** only the 10 dirty pages are scanned (not all 1000 pages).
2. **Given** a workload with frequent minor collections, **When** benchmarking pause times, **Then** minor GC pauses are 2-5x faster than the baseline implementation.

---

### User Story 2 - Minimal Write Barrier Overhead (Priority: P2)

As a developer, I want the write barrier overhead to remain minimal so that mutation-heavy workloads do not experience significant performance degradation.

**Why this priority**: The write barrier is called on every mutation to old-generation objects. Excessive overhead here would negate the benefits of faster minor GC pauses.

**Independent Test**: Can be fully tested by running microbenchmarks that measure write barrier cost in isolation, verifying overhead is less than 5% compared to the current implementation.

**Acceptance Scenarios**:

1. **Given** an old-generation object being mutated, **When** the write barrier fires, **Then** the operation completes with minimal overhead (atomic operations only, no serialization).
2. **Given** multiple threads mutating the same old-generation object concurrently, **When** write barriers fire simultaneously, **Then** no excessive contention occurs.

---

### User Story 3 - Correct Old-to-Young Reference Survival (Priority: P1)

As a developer, I want objects in the young generation that are referenced by mutated old-generation objects to survive minor GC correctly, ensuring no use-after-free or dangling references.

**Why this priority**: Correctness is non-negotiable. If dirty page tracking fails to identify old-to-young references, young objects may be incorrectly collected, causing memory safety violations.

**Independent Test**: Can be fully tested by creating old-to-young reference scenarios, triggering minor GC, and verifying all referenced young objects survive.

**Acceptance Scenarios**:

1. **Given** an old-generation object that mutates to reference a young-generation object, **When** minor GC runs, **Then** the young object survives because the dirty page was scanned.
2. **Given** an old-generation large object (>2KB) that references young objects, **When** minor GC runs, **Then** all referenced young objects survive.

---

### User Story 4 - Thread-Safe Concurrent Access (Priority: P2)

As a developer using rudo-gc in a multi-threaded application, I want dirty page tracking to work correctly across threads without race conditions or data corruption.

**Why this priority**: Multi-threaded applications are a core use case. The dirty page list must handle concurrent write barriers and GC operations safely.

**Independent Test**: Can be fully tested by running concurrent mutation tests and loom-based concurrency verification.

**Acceptance Scenarios**:

1. **Given** multiple threads mutating different old-generation objects, **When** a minor GC is triggered, **Then** all dirty pages are correctly identified and scanned.
2. **Given** a thread adding to the dirty page list while GC takes a snapshot, **When** the snapshot is taken, **Then** no pages are lost and no races occur.

---

### Edge Cases

- What happens when the dirty page list is empty? (No dirty pages to scan - minor GC should complete quickly without errors)
- How does the system handle a page that becomes dirty immediately after the snapshot is taken? (Page will be added to the new dirty list for the next GC cycle)
- What happens when a page is promoted (young→old) with existing dirty bits? (Dirty bits are preserved; page will be caught by next write barrier)
- How are large objects (>2KB) handled? (Traced as a single unit when their page is in the dirty list)
- What happens if the same page is mutated multiple times between GCs? (Page appears in dirty list exactly once due to deduplication)

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST maintain a list of pages containing dirty objects in the old generation
- **FR-002**: System MUST add pages to the dirty list atomically when write barriers fire for old-generation mutations
- **FR-003**: System MUST prevent duplicate entries in the dirty page list (each dirty page appears exactly once)
- **FR-004**: System MUST scan only dirty pages during minor GC instead of iterating all pages
- **FR-005**: System MUST clear the dirty page list and associated flags after each minor GC cycle
- **FR-006**: System MUST support concurrent access to the dirty page list from multiple mutator threads
- **FR-007**: System MUST take a snapshot of the dirty page list at GC start to avoid holding locks during scanning
- **FR-008**: System MUST handle large objects (>2KB) by tracing the entire object when its page is dirty
- **FR-009**: System MUST preserve existing per-object dirty bitmap functionality for object-level precision
- **FR-010**: System MUST track dirty page statistics for capacity planning and debugging

### Key Entities

- **Dirty Page List**: A mutex-protected collection of page pointers that have dirty objects. Cleared after each minor GC.
- **Page Header Flag (DIRTY_LISTED)**: An atomic flag indicating whether a page is already in the dirty list, preventing duplicates.
- **Dirty Pages Snapshot**: A per-GC-cycle copy of the dirty page list, allowing lock-free scanning during GC.
- **Dirty Bitmap**: Existing per-page bitmap tracking which objects within a page have been modified.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Minor GC pause times are reduced by 2-5x for heaps with many old-generation pages but few dirty pages
- **SC-002**: Write barrier overhead increases by less than 5% compared to current implementation
- **SC-003**: Memory overhead for dirty page tracking is less than 0.1% of total heap size
- **SC-004**: Page scan complexity is O(dirty_pages) instead of O(all_pages) for minor GC
- **SC-005**: All existing tests pass (including Miri memory safety checks)
- **SC-006**: No race conditions detected in loom-based concurrency tests
- **SC-007**: No memory leaks or use-after-free conditions in Miri tests

## Assumptions

- The existing 2-generation model (young/old) remains unchanged
- Page-level promotion continues to be used (not object-level)
- The parking_lot crate is available for mutex implementation
- Typical workloads have 10-100 dirty pages per minor GC cycle
- Large objects are relatively rare in typical workloads

## Dependencies

- Existing generation field in PageHeader (already implemented)
- Existing write barrier mechanism in GcCell (already implemented)
- Existing per-object dirty bitmap (already implemented)
- Existing minor collection orchestration (already implemented)

## Out of Scope

- Card table implementation (rejected in favor of dirty page list for current 2-gen design)
- Support for more than 2 generations (future enhancement)
- Object-level promotion (current page-level promotion is retained)
- Incremental marking during GC (separate feature)
