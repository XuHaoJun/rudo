# Feature Specification: Incremental Marking for Major GC

**Feature Branch**: `008-incremental-marking`  
**Created**: 2026-02-03  
**Status**: Draft  
**Input**: User description: "Implement incremental marking to reduce major GC pause times by splitting the mark phase into smaller cooperative increments that interleave with mutator execution, achieving 50-80% reduction in pause times for large heaps"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Reduced GC Pause Times for Large Heaps (Priority: P1)

Applications using rudo-gc with large heaps (1GB+) experience long pause times during major garbage collection. Currently, the entire mark phase runs in a stop-the-world (STW) pause, which can take 100ms or more. This causes noticeable application freezes, degraded user experience, and potential timeout issues in latency-sensitive applications.

With incremental marking, the mark phase is split into smaller increments that interleave with mutator execution. Applications continue running during most of the marking work, experiencing only brief pauses for snapshot and finalization phases.

**Why this priority**: This is the core value proposition - reducing pause times is the primary goal of incremental marking. Without this, the feature provides no benefit.

**Independent Test**: Can be fully tested by measuring pause times during major GC on a large heap. The test allocates 1GB of objects, triggers a major collection, and measures the maximum pause time. The feature delivers value if pause times are reduced by at least 50% compared to STW marking.

**Acceptance Scenarios**:

1. **Given** an application with a 1GB heap containing live objects, **When** a major GC is triggered, **Then** the maximum pause time is under 10ms
2. **Given** an application running during incremental marking, **When** mutators modify object references, **Then** all reachable objects are correctly preserved and no objects are incorrectly collected
3. **Given** an application with multiple threads, **When** incremental marking runs concurrently with mutator threads, **Then** mutator threads can continue executing for at least 90% of the marking duration

---

### User Story 2 - Correctness Under Concurrent Mutation (Priority: P1)

During incremental marking, mutator threads continue running and can modify object references. These modifications must be tracked to ensure no reachable objects are lost. The system must correctly handle cases where objects are created, references are overwritten, or references are deleted during the mark phase.

**Why this priority**: Correctness is non-negotiable - losing objects would cause use-after-free bugs and memory corruption. This must work correctly for the feature to be viable.

**Independent Test**: Can be fully tested using concurrent mutation tests. The test starts incremental marking, has mutator threads modify references during marking, and verifies all reachable objects survive collection. The feature delivers value by maintaining memory safety guarantees.

**Acceptance Scenarios**:

1. **Given** incremental marking is in progress, **When** a mutator overwrites a reference to an object, **Then** the overwritten object is still marked if it remains reachable through other paths
2. **Given** incremental marking is in progress, **When** a mutator creates new objects, **Then** new objects are immediately marked to prevent them from being collected
3. **Given** incremental marking is in progress, **When** multiple threads concurrently modify references, **Then** all modifications are correctly tracked and no objects are lost

---

### User Story 3 - Graceful Fallback Under High Mutation Rates (Priority: P2)

When mutation rates are very high, the system may accumulate too many dirty pages or exceed resource budgets. In these cases, the system should gracefully fall back to stop-the-world marking rather than continuing indefinitely or failing.

**Why this priority**: Ensures the system remains robust under extreme conditions and prevents unbounded resource consumption.

**Independent Test**: Can be fully tested by creating a high-mutation workload during incremental marking and verifying the system falls back to STW when thresholds are exceeded. The feature delivers value by maintaining system stability under stress.

**Acceptance Scenarios**:

1. **Given** incremental marking is in progress, **When** the number of dirty pages exceeds a threshold, **Then** the system completes marking with a stop-the-world pause
2. **Given** incremental marking is in progress, **When** marking takes longer than a timeout threshold, **Then** the system completes marking with a stop-the-world pause
3. **Given** incremental marking falls back to STW, **When** the fallback completes, **Then** all objects are correctly marked and the collection proceeds normally

---

### Edge Cases

- What happens when incremental marking is interrupted by a minor GC request?
- How does the system handle very deep object graphs during incremental marking?
- What happens when all mutator threads are blocked during incremental marking?
- How does the system handle rapid allocation bursts during incremental marking?
- What happens when write barrier buffers overflow?
- How does the system coordinate multiple marking workers when they have different progress rates?

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST split major GC marking into smaller increments that interleave with mutator execution
- **FR-002**: System MUST track object modifications during incremental marking to prevent lost objects
- **FR-003**: System MUST mark newly allocated objects immediately during incremental marking to prevent premature collection
- **FR-004**: System MUST complete incremental marking only when all reachable objects are marked and all dirty pages are processed
- **FR-005**: System MUST fall back to stop-the-world marking when resource thresholds are exceeded
- **FR-006**: System MUST coordinate multiple marking workers to ensure consistent progress
- **FR-007**: System MUST integrate with existing dirty page list generational GC without breaking its functionality
- **FR-008**: System MUST maintain correctness under concurrent mutation from multiple threads
- **FR-009**: System MUST prevent minor GC from running during incremental major marking
- **FR-010**: System MUST allow mutator threads to continue executing for the majority of the marking duration

### Key Entities *(include if feature involves data)*

- **Incremental Marking State**: Tracks the current phase of incremental marking (idle, snapshot, marking, final mark, sweeping) and coordinates between marking workers and mutators
- **Dirty Page Snapshot**: A snapshot of pages modified during incremental marking that need to be rescanned to find new references
- **Marking Worklist**: A queue of objects that need to be marked, processed incrementally during marking slices
- **Write Barrier Record**: Records of reference modifications that occurred during incremental marking to ensure correctness

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Maximum pause time during major GC for a 1GB heap is reduced to under 10ms (compared to 100ms+ with STW marking)
- **SC-002**: Mutator threads can execute for at least 90% of the total marking duration during incremental marking
- **SC-003**: No objects are lost during incremental marking under concurrent mutation (verified by concurrent mutation tests)
- **SC-004**: Total GC time for incremental marking does not exceed 2x the time required for stop-the-world marking
- **SC-005**: Write barrier overhead adds less than 10% performance cost compared to generational GC alone
- **SC-006**: System correctly falls back to STW marking within 1 second when resource thresholds are exceeded
- **SC-007**: All existing GC tests pass without modification, ensuring backward compatibility

---

## Breaking API Changes

This section documents API changes introduced in v0.7.0.

### v0.7.0: GcCell API Redesign

The `GcCell` API has been redesigned to be backward compatible while maintaining correctness.

**Key Changes**:
- `borrow_mut()` now requires `T: Trace` instead of `T: GcCapture`
- Added `borrow_mut_with_satb()` for types requiring SATB barrier
- Added `borrow_mut_gen_only()` for performance optimization

### API Comparison

| Method                   | T Bound   | Barrier Type              | Use Case                          |
|--------------------------|-----------|---------------------------|-----------------------------------|
| `borrow_mut()`           | `Trace`   | Generational + Incremental| General use (recommended)         |
| `borrow_mut_with_satb()` | `GcCapture` | Full (incl. SATB)       | Types with GC pointers            |
| `borrow_mut_gen_only()`  | -         | Generational only         | Performance optimization          |

### Before vs After

| Aspect | v0.6.x | v0.7.x |
|--------|--------|--------|
| `GcCell<i32>::borrow_mut()` | ✅ Works | ✅ Works |
| `GcCell<Gc<T>>::borrow_mut()` | ✅ Works | ✅ Works |
| SATB for GC pointers | Automatic | Opt-in via `borrow_mut_with_satb()` |

### Migration Guide

```rust
// Case 1: GcCell<Gc<T>> - Works with both methods
let cell = GcCell::new(Gc::new(Data));
*cell.borrow_mut() = new_data;              // Generational + Incremental (recommended)
*cell.borrow_mut_with_satb() = new_data;    // Full (explicit SATB)

// Case 2: GcCell<i32> - Works with borrow_mut()
let cell = GcCell::new(42);
*cell.borrow_mut() = 100;  // Works! (generational + incremental barrier)

// Case 3: Performance optimization
let cell = GcCell::new(expensive_computation());
*cell.borrow_mut_gen_only() = result;  // Generational barrier only
```

### Why This Design?

1. **Backward Compatible**: Existing code continues to work
2. **Correctness by Default**: `borrow_mut()` provides generational + incremental barriers
3. **Opt-in SATB**: `borrow_mut_with_satb()` for types requiring SATB
4. **Performance Path**: `borrow_mut_gen_only()` for hot paths
