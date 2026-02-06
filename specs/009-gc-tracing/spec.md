# Feature Specification: GC Tracing Observability

**Feature Branch**: `009-gc-tracing`  
**Created**: 2026-02-05  
**Status**: Draft  
**Input**: User description: "Add optional tracing feature for GC observability using the tracing crate, providing structured spans and events for garbage collection phases (clear, mark, sweep) with zero-cost when disabled"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Basic GC Collection Tracing (Priority: P1)

As a developer using rudo-gc, I want to observe when garbage collections occur and their outcomes, so that I can understand GC behavior and performance characteristics in my application.

**Why this priority**: This is the foundational use case that provides immediate value. Without basic collection visibility, users cannot diagnose GC-related performance issues or understand memory management patterns.

**Independent Test**: Can be fully tested by enabling the `tracing` feature, configuring a tracing subscriber, and verifying that GC collection events appear in logs with correct metadata (collection type, duration, bytes reclaimed).

**Acceptance Scenarios**:

1. **Given** an application using rudo-gc with the `tracing` feature enabled, **When** a garbage collection runs, **Then** a `gc_collect` span is created with the collection type (minor, major_single_threaded, major_multi_threaded)
2. **Given** a tracing subscriber configured at DEBUG level, **When** GC completes, **Then** the span includes duration and bytes_reclaimed metrics
3. **Given** the `tracing` feature is disabled, **When** the application compiles and runs, **Then** no tracing code is generated (zero-cost abstraction)

---

### User Story 2 - Phase-Level Tracing (Priority: P2)

As a developer debugging GC performance, I want to see which phases (clear, mark, sweep) are taking time during collections, so that I can identify bottlenecks and optimize memory usage patterns.

**Why this priority**: Phase-level visibility enables performance tuning and debugging. While less critical than basic collection tracking, it provides essential diagnostic information for optimization work.

**Independent Test**: Can be tested by running a GC collection and verifying that phase_start and phase_end events are logged for each phase (clear, mark, sweep) with accurate byte counts.

**Acceptance Scenarios**:

1. **Given** a major garbage collection is triggered, **When** the clear phase begins, **Then** a `phase_start` event is logged with phase="clear" and bytes_before count
2. **Given** the mark phase is running, **When** marking completes, **Then** a `phase_end` event is logged with objects_marked count
3. **Given** a collection spans multiple phases, **Then** each phase is clearly distinguishable in the trace output with proper parent-child span relationships

---

### User Story 3 - Incremental Marking Tracing (Priority: P3)

As a developer using incremental marking to reduce pause times, I want to observe incremental marking slices and fallback events, so that I can verify the incremental GC is working effectively and understand when it falls back to stop-the-world.

**Why this priority**: This supports advanced users who rely on incremental marking. It provides visibility into a complex subsystem, but is not required for basic GC observability.

**Independent Test**: Can be tested by enabling incremental marking, triggering allocations that cause mark slices, and verifying that `incremental_slice` events appear with objects_marked and dirty_pages counts.

**Acceptance Scenarios**:

1. **Given** incremental marking is enabled and active, **When** a mark slice completes, **Then** an `incremental_slice` event is logged with objects_marked and dirty_pages metrics
2. **Given** incremental marking exceeds budget or timeout, **When** fallback to stop-the-world occurs, **Then** a `fallback` event is logged with the reason
3. **Given** the final mark phase executes, **When** marking completes, **Then** a span captures the final mark duration and objects marked

---

### Edge Cases

- **Zero-cost when disabled**: When the `tracing` feature is not enabled, no tracing spans or events should be compiled into the binary
- **Multi-threaded span propagation**: In multi-threaded collections, spans must be properly propagated to worker threads without corruption or interleaving
- **High-frequency collections**: Tracing should not cause performance degradation even during high-frequency GC cycles
- **Error scenarios**: If tracing system fails (e.g., subscriber not configured), GC should continue to function normally
- **Memory pressure**: Tracing metadata should not contribute significantly to memory overhead during GC operations

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide an optional `tracing` feature flag that can be enabled in Cargo.toml
- **FR-002**: When `tracing` feature is disabled, no tracing code MUST be generated (zero-cost abstraction)
- **FR-003**: System MUST create a `gc_collect` span for each garbage collection with collection_type attribute
- **FR-004**: System MUST generate `phase_start` and `phase_end` events for clear, mark, and sweep phases
- **FR-005**: System MUST include bytes_before and bytes_reclaimed metrics in phase events
- **FR-006**: System MUST support tracing for all collection types: minor, major_single_threaded, major_multi_threaded
- **FR-007**: System MUST create an `incremental_mark` span during incremental marking slices
- **FR-008**: System MUST log `incremental_slice` events with objects_marked and dirty_pages counts
- **FR-009**: System MUST log `fallback` events when incremental marking falls back to stop-the-world with reason
- **FR-010**: System MUST use DEBUG log level for all tracing to minimize overhead when enabled
- **FR-011**: System MUST provide a stable `GcId` identifier for correlating events within a single collection
- **FR-012**: Spans MUST be properly propagated across worker threads in multi-threaded collections

### Key Entities *(include if feature involves data)*

- **GcPhase**: Represents high-level GC phases (Clear, Mark, Sweep) for categorizing trace events
- **GcId**: A unique identifier for each garbage collection run, enabling correlation of related events
- **CollectionType**: The type of collection being performed (minor, major_single_threaded, major_multi_threaded)

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Users can enable tracing by adding `tracing` feature to Cargo.toml without code changes
- **SC-002**: GC events appear in traces within 100ms of collection completion when subscriber is configured
- **SC-003**: Binary size increase when tracing is disabled MUST be 0 bytes (verified by comparing builds)
- **SC-004**: Runtime overhead when tracing is enabled but subscriber filters out debug logs MUST be less than 1%
- **SC-005**: All major, minor, and incremental collections produce traceable events with proper span hierarchy
- **SC-006**: Phase-level events provide sufficient detail to identify which phase dominates collection time
- **SC-007**: Multi-threaded collections show consistent span parent-child relationships across all worker threads

## Assumptions

- Users who enable tracing will configure a compatible `tracing_subscriber` in their application
- DEBUG log level is appropriate for GC tracing to avoid spamming default INFO-level logs
- The `tracing` crate version 0.1.x is acceptable as a dependency (matches ecosystem standards)
- Phase-level granularity provides sufficient detail for most debugging scenarios without excessive verbosity
- Span propagation across threads can be achieved by capturing and entering spans in worker closures

## Dependencies

- **tracing crate**: Optional dependency (version 0.1) for structured logging
- **Existing GC infrastructure**: Clear, mark, and sweep phases must expose hooks for tracing integration
- **Metrics system**: Existing `CollectionType` and metrics infrastructure for trace attributes

## Out of Scope

- Real-time metrics export (prometheus, statsd, etc.) - use external subscriber
- Custom trace formatting or visualization tools
- Integration with distributed tracing systems (OpenTelemetry, Jaeger)
- Conversion of existing `eprintln!` debug output to tracing (optional future enhancement)
- Per-object allocation tracing (too high overhead)
- Heap visualization or object graph dumping
