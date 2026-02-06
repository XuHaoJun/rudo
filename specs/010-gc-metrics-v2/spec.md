# Feature Specification: Extended GC Metrics System

**Feature Branch**: `010-gc-metrics-v2`  
**Created**: 2026-02-06  
**Status**: Draft  
**Input**: User description: "Extend GC metrics system to provide phase-level timing breakdown, incremental marking statistics, cumulative cross-thread statistics, real-time heap queries, and GC history for trend analysis"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Identify Slow GC Phases (Priority: P1)

Developers need to understand which phase of garbage collection (clear, mark, or sweep) is causing performance bottlenecks. Currently, they only see total GC duration, making it impossible to optimize the specific phase that's slow.

**Why this priority**: Performance optimization requires knowing where time is spent. Without phase-level breakdown, developers cannot make informed decisions about which GC algorithms or optimizations to focus on.

**Independent Test**: Can be fully tested by querying metrics after a GC cycle and verifying that phase durations are reported and sum approximately to total duration. Delivers immediate value for performance analysis.

**Acceptance Scenarios**:

1. **Given** a GC cycle has completed, **When** a developer queries the last GC metrics, **Then** they receive separate timing information for clear, mark, and sweep phases
2. **Given** a minor collection occurs (which skips clear phase), **When** metrics are queried, **Then** clear phase duration is zero while mark and sweep durations are reported
3. **Given** phase timings are reported, **When** a developer sums the phase durations, **Then** the sum is approximately equal to total GC duration (within reasonable overhead tolerance)

---

### User Story 2 - Monitor Incremental Marking Behavior (Priority: P1)

Developers using incremental marking need visibility into how the incremental algorithm is performing, including whether it's falling back to stop-the-world collection and why.

**Why this priority**: Incremental marking is a complex feature that can fail silently. Developers need to know if incremental marking is working as intended or if fallbacks are occurring, which defeats the purpose of incremental collection.

**Independent Test**: Can be fully tested by enabling incremental marking, triggering a major collection, and verifying that incremental statistics (slices executed, dirty pages scanned, fallback status) are reported. Delivers value for understanding incremental GC effectiveness.

**Acceptance Scenarios**:

1. **Given** incremental marking is enabled and a major collection occurs, **When** metrics are queried, **Then** incremental marking statistics (objects marked, slices executed, dirty pages scanned) are reported
2. **Given** incremental marking falls back to stop-the-world, **When** metrics are queried, **Then** fallback status and reason are reported
3. **Given** a non-incremental collection occurs, **When** metrics are queried, **Then** incremental marking fields report zero or false values

---

### User Story 3 - Track Cumulative GC Statistics (Priority: P1)

Developers need to understand GC behavior over the lifetime of their application, not just the last collection. This includes total collections, total pause time, and total memory reclaimed across all threads.

**Why this priority**: Application-level performance analysis requires aggregate statistics. Per-collection snapshots don't reveal long-term trends or cumulative impact on application performance.

**Independent Test**: Can be fully tested by performing multiple GC cycles and verifying that cumulative counters increment correctly and reflect totals across all collections. Delivers value for application-level performance monitoring.

**Acceptance Scenarios**:

1. **Given** multiple GC cycles have occurred, **When** a developer queries global metrics, **Then** they receive cumulative totals for collections, pause time, and memory reclaimed
2. **Given** collections occur on multiple threads, **When** global metrics are queried, **Then** statistics reflect aggregates across all threads
3. **Given** different collection types occur (minor, major, incremental), **When** global metrics are queried, **Then** separate counters for each collection type are available

---

### User Story 4 - Query Heap State Without Triggering GC (Priority: P1)

Developers need to inspect current heap allocation state (total size, young generation size, old generation size) without forcing a garbage collection cycle.

**Why this priority**: Debugging memory issues requires understanding current heap state. Currently, developers must trigger GC to get any metrics, which interferes with debugging and can mask memory problems.

**Independent Test**: Can be fully tested by allocating objects, querying heap size, and verifying that reported sizes reflect current allocations without triggering collection. Delivers immediate value for memory debugging.

**Acceptance Scenarios**:

1. **Given** objects are allocated on the heap, **When** a developer queries current heap size, **Then** they receive the total allocated bytes without triggering GC
2. **Given** objects are allocated, **When** a developer queries young and old generation sizes, **Then** they receive separate sizes for each generation
3. **Given** no heap is initialized on a thread, **When** heap queries are made, **Then** they return zero or handle the absence gracefully

---

### User Story 5 - Analyze GC History Trends (Priority: P2)

Developers need to analyze GC performance trends over recent collections to detect regressions, compute averages, and identify patterns in pause times.

**Why this priority**: While less critical than real-time visibility, trend analysis enables detection of performance regressions and helps establish performance baselines. This is valuable for long-running applications and performance testing.

**Independent Test**: Can be fully tested by performing multiple collections, querying history, and verifying that recent collections are accessible and statistical functions (average, max) work correctly. Delivers value for performance regression detection.

**Acceptance Scenarios**:

1. **Given** multiple GC cycles have occurred, **When** a developer queries GC history, **Then** they can access metrics from recent collections (up to a reasonable limit)
2. **Given** GC history is available, **When** a developer requests average pause time over recent collections, **Then** they receive the calculated average
3. **Given** GC history is available, **When** a developer requests maximum pause time over recent collections, **Then** they receive the maximum value
4. **Given** more collections occur than history capacity, **When** history is queried, **Then** only the most recent collections are retained (oldest are discarded)

---

### Edge Cases

- What happens when metrics are queried before any GC has occurred? (Should return zero/default values)
- How does the system handle concurrent queries to global metrics from multiple threads? (Should be thread-safe and provide consistent snapshots)
- What happens when heap queries are made from a thread that doesn't have a heap initialized? (Should return zero or handle gracefully)
- How does the system handle very long-running applications where cumulative counters might overflow? (Should use appropriate integer sizes or document limits)
- What happens when GC history capacity is exceeded? (Should wrap around, keeping most recent entries)
- How does phase timing behave during concurrent multi-threaded collections? (Should aggregate timing across threads appropriately)

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide separate timing information for clear, mark, and sweep phases in GC metrics
- **FR-002**: System MUST include incremental marking statistics (objects marked, slices executed, dirty pages scanned) in GC metrics when incremental marking is used
- **FR-003**: System MUST report fallback status and reason when incremental marking falls back to stop-the-world collection
- **FR-004**: System MUST provide cumulative statistics (total collections, total pause time, total memory reclaimed) across all GC cycles and threads
- **FR-005**: System MUST provide separate cumulative counters for minor, major, and incremental major collection types
- **FR-006**: System MUST allow querying current heap size (total allocated bytes) without triggering garbage collection
- **FR-007**: System MUST allow querying current young generation size without triggering garbage collection
- **FR-008**: System MUST allow querying current old generation size without triggering garbage collection
- **FR-009**: System MUST maintain a history of recent GC metrics (up to a configurable limit)
- **FR-010**: System MUST provide functions to compute average pause time over recent collections from history
- **FR-011**: System MUST provide functions to compute maximum pause time over recent collections from history
- **FR-012**: System MUST ensure all metrics queries are thread-safe and can be called from any thread
- **FR-013**: System MUST maintain backward compatibility with existing metrics API (existing fields unchanged, new fields optional/default to zero)

### Key Entities

- **GC Metrics**: Per-collection snapshot containing timing, memory, and object statistics. Extended with phase timings and incremental marking data.
- **Global Metrics**: Process-level cumulative statistics aggregating data across all collections and threads. Includes counters for total collections, pause time, memory reclaimed, and collection type breakdowns.
- **GC History**: Storage of recent GC metrics snapshots, enabling trend analysis and statistical computation over recent collections.
- **Heap State**: Current allocation state of the heap, including total size and generation-specific sizes (young/old), queryable without triggering collection.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Developers can identify which GC phase (clear, mark, or sweep) accounts for the majority of pause time in 100% of cases where phase timing is available
- **SC-002**: Developers can determine if incremental marking is functioning correctly (not falling back unexpectedly) by inspecting metrics after every incremental collection
- **SC-003**: Developers can track cumulative GC impact (total pause time, total collections) over application lifetime with accuracy within 1% of actual values
- **SC-004**: Developers can query current heap state without triggering collection, with query latency under 1 microsecond on average
- **SC-005**: Developers can analyze GC performance trends over a configurable number of recent collections with 100% data availability for the retained history window
- **SC-006**: All new metrics features maintain backward compatibility - existing code using metrics API continues to work without modification
- **SC-007**: Metrics collection adds less than 1% overhead to GC pause times (measured on typical workloads)
