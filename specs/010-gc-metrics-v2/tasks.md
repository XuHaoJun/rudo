# Tasks: Extended GC Metrics System

**Input**: Design documents from `/specs/010-gc-metrics-v2/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Additional Reference**: For detailed code examples, test implementations, and integration patterns, see `docs/metrics-improvement-plan-v2.md`:
- **Section 4.1**: Complete code examples for instrumenting collection functions with `PhaseTimer`
- **Section 8**: Full test code examples (unit and integration tests)
- **Section 6**: Usage examples demonstrating the public API
- **Section 2**: Architecture constraints and design rationale

**Tests**: Tests are included as they are standard practice for Rust projects and required by the constitution.

**Organization**: Tasks are organized by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Single Rust crate**: `crates/rudo-gc/src/`, `crates/rudo-gc/tests/`
- All paths relative to repository root `/home/noah/Desktop/rudo/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and verification

- [x] T001 Verify existing `crates/rudo-gc/src/metrics.rs` structure and current `GcMetrics` definition
- [x] T002 Verify existing `crates/rudo-gc/src/gc/gc.rs` collection functions and `record_metrics()` call sites
- [x] T003 [P] Verify `FallbackReason` enum exists in `crates/rudo-gc/src/gc/incremental.rs` and is accessible
- [x] T004 [P] Verify `MarkStats` struct exists in `crates/rudo-gc/src/gc/incremental.rs` and `IncrementalMarkState::global().stats()` access pattern

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [x] T005 [US1] [US2] Add 8 new fields to `GcMetrics` struct in `crates/rudo-gc/src/metrics.rs` (clear_duration, mark_duration, sweep_duration, objects_marked, dirty_pages_scanned, slices_executed, fallback_occurred, fallback_reason)
- [x] T006 [US1] [US2] Update `GcMetrics::new()` to initialize new fields to zero/false/`FallbackReason::None` in `crates/rudo-gc/src/metrics.rs`
- [x] T007 [US1] [US2] Re-export `FallbackReason` from `gc::incremental` in `crates/rudo-gc/src/metrics.rs`
- [x] T008 [US1] [US2] Add `PhaseTimer` struct with `new()`, `start()`, `end_clear()`, `end_mark()`, `end_sweep()` methods in `crates/rudo-gc/src/metrics.rs`
- [x] T009 [US1] [US2] Add `CollectResult` struct (objects_reclaimed, timer, collection_type) in `crates/rudo-gc/src/gc/gc.rs`

**Checkpoint**: Foundation ready - user story implementation can now begin

---

## Phase 3: User Story 1 - Identify Slow GC Phases (Priority: P1) 🎯 MVP

**Goal**: Developers can identify which GC phase (clear, mark, or sweep) accounts for the majority of pause time

**Independent Test**: Query metrics after a GC cycle and verify phase durations are reported and sum approximately to total duration

### Tests for User Story 1

- [x] T010 [P] [US1] Add unit test `test_gc_metrics_new_fields_default_to_zero` in `crates/rudo-gc/src/metrics.rs`
- [x] T011 [P] [US1] Add unit test `test_phase_timer_captures_durations` in `crates/rudo-gc/src/metrics.rs`
- [x] T012 [P] [US1] Add integration test `test_phase_timing_sums_approximately` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T013 [P] [US1] Add integration test `test_minor_collection_clear_duration_zero` in `crates/rudo-gc/tests/metrics_tests.rs`

### Implementation for User Story 1

- [x] T014 [US1] Change `collect_major_stw()` return type to `CollectResult` and add `PhaseTimer` instrumentation in `crates/rudo-gc/src/gc/gc.rs`
- [x] T015 [US1] Change `collect_major_incremental()` return type to `CollectResult` and add `PhaseTimer` instrumentation in `crates/rudo-gc/src/gc/gc.rs`
- [x] T016 [US1] Add `PhaseTimer` instrumentation to `collect_minor()` function (minor collections skip clear phase, combine mark+sweep) in `crates/rudo-gc/src/gc/gc.rs`
- [x] T017 [US1] Update `collect_major()` to return `CollectResult` and propagate from inner functions in `crates/rudo-gc/src/gc/gc.rs`
- [x] T018 [US1] Add `PhaseTimer` instrumentation to `perform_multi_threaded_collect()` and populate `GcMetrics` phase timing fields in `crates/rudo-gc/src/gc/gc.rs`
- [x] T019 [US1] Add `PhaseTimer` instrumentation to `perform_multi_threaded_collect_full()` and populate `GcMetrics` phase timing fields in `crates/rudo-gc/src/gc/gc.rs`
- [x] T020 [US1] Add `PhaseTimer` instrumentation to `perform_single_threaded_collect_with_wake()` using `CollectResult` from `collect_major()` and handle `collect_minor()` case in `crates/rudo-gc/src/gc/gc.rs`
- [x] T021 [US1] Add `PhaseTimer` instrumentation to `perform_single_threaded_collect_full()` using `CollectResult` from `collect_major()` in `crates/rudo-gc/src/gc/gc.rs`
- [x] T022 [US1] Update `lib.rs` re-exports to include `FallbackReason` in `crates/rudo-gc/src/lib.rs`

**Checkpoint**: At this point, User Story 1 should be fully functional - phase durations are reported for all collection types ✓ COMPLETED

---

## Phase 4: User Story 2 - Monitor Incremental Marking Behavior (Priority: P1)

**Goal**: Developers can determine if incremental marking is functioning correctly and see fallback status

**Independent Test**: Enable incremental marking, trigger major collection, verify incremental statistics are reported

### Tests for User Story 2

- [x] T023 [P] [US2] Add integration test `test_incremental_metrics_populated` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T024 [P] [US2] Add integration test `test_fallback_reason_reported` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T025 [P] [US2] Add integration test `test_non_incremental_fields_zero` in `crates/rudo-gc/tests/metrics_tests.rs`

### Implementation for User Story 2

- [x] T026 [US2] Read `MarkStats` atomics in `collect_major_incremental()` and populate `CollectResult` with incremental stats in `crates/rudo-gc/src/gc/gc.rs`
- [x] T027 [US2] Set `collection_type` to `IncrementalMajor` in `collect_major_incremental()` return value in `crates/rudo-gc/src/gc/gc.rs`
- [x] T028 [US2] Read `MarkStats` atomics in `perform_multi_threaded_collect()` and populate `GcMetrics` incremental fields in `crates/rudo-gc/src/gc/gc.rs`
- [x] T029 [US2] Read `MarkStats` atomics in `perform_multi_threaded_collect_full()` and populate `GcMetrics` incremental fields in `crates/rudo-gc/src/gc/gc.rs`
- [x] T030 [US2] Read `MarkStats` atomics in `perform_single_threaded_collect_with_wake()` and populate `GcMetrics` incremental fields in `crates/rudo-gc/src/gc/gc.rs`
- [x] T031 [US2] Read `MarkStats` atomics in `perform_single_threaded_collect_full()` and populate `GcMetrics` incremental fields in `crates/rudo-gc/src/gc/gc.rs`

**Checkpoint**: At this point, User Story 2 should be fully functional - incremental marking stats are visible in metrics

---

## Phase 5: User Story 3 - Track Cumulative GC Statistics (Priority: P1)

**Goal**: Developers can track cumulative GC impact over application lifetime across all threads

**Independent Test**: Perform multiple GC cycles and verify cumulative counters increment correctly

### Tests for User Story 3

- [x] T032 [P] [US3] Add unit test `test_global_metrics_new` in `crates/rudo-gc/src/metrics.rs`
- [x] T033 [P] [US3] Add integration test `test_global_metrics_accumulate` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T034 [P] [US3] Add integration test `test_global_metrics_multi_threaded` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T035 [P] [US3] Add integration test `test_global_metrics_collection_type_breakdown` in `crates/rudo-gc/tests/metrics_tests.rs`

### Implementation for User Story 3

- [x] T036 [US3] Add `GlobalMetrics` struct with 8 atomic fields (total_collections, total_minor_collections, total_major_collections, total_incremental_collections, total_bytes_reclaimed, total_objects_reclaimed, total_pause_ns, total_fallbacks) in `crates/rudo-gc/src/metrics.rs`
- [x] T037 [US3] Add `GlobalMetrics::new()` const constructor initializing all atomics to zero in `crates/rudo-gc/src/metrics.rs`
- [x] T038 [US3] Add static singleton `GLOBAL_METRICS: GlobalMetrics` in `crates/rudo-gc/src/metrics.rs`
- [x] T039 [US3] Add `global_metrics()` accessor function returning `&'static GlobalMetrics` in `crates/rudo-gc/src/metrics.rs`
- [x] T040 [US3] Add 8 read accessor methods to `GlobalMetrics` impl (all `#[inline]`, `#[must_use]`, `Relaxed` ordering) in `crates/rudo-gc/src/metrics.rs`
- [x] T041 [US3] Update `record_metrics()` to increment `GLOBAL_METRICS` counters after thread-local update in `crates/rudo-gc/src/metrics.rs`
- [x] T042 [US3] Update `lib.rs` re-exports to include `global_metrics` and `GlobalMetrics` in `crates/rudo-gc/src/lib.rs`

**Checkpoint**: At this point, User Story 3 should be fully functional - cumulative statistics are available via `global_metrics()`

---

## Phase 6: User Story 4 - Query Heap State Without Triggering GC (Priority: P1)

**Goal**: Developers can inspect current heap allocation state without forcing a GC cycle

**Independent Test**: Allocate objects, query heap size, verify reported sizes reflect current allocations without triggering collection

### Tests for User Story 4

- [x] T043 [P] [US4] Add integration test `test_heap_queries_return_sane_values` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T044 [P] [US4] Add integration test `test_heap_queries_no_heap_returns_zero` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T045 [P] [US4] Add integration test `test_heap_queries_young_old_generations` in `crates/rudo-gc/tests/metrics_tests.rs`

### Implementation for User Story 4

- [x] T046 [US4] Add `current_heap_size()` function reading `HEAP.try_with(|h| ... .total_allocated())` in `crates/rudo-gc/src/metrics.rs`
- [x] T047 [US4] Add `current_young_size()` function reading `HEAP.try_with(|h| ... .young_allocated())` in `crates/rudo-gc/src/metrics.rs`
- [x] T048 [US4] Add `current_old_size()` function reading `HEAP.try_with(|h| ... .old_allocated())` in `crates/rudo-gc/src/metrics.rs`
- [x] T049 [US4] Update `lib.rs` re-exports to include `current_heap_size`, `current_young_size`, `current_old_size` in `crates/rudo-gc/src/lib.rs`

**Checkpoint**: At this point, User Story 4 should be fully functional - heap queries work without triggering GC

---

## Phase 7: User Story 5 - Analyze GC History Trends (Priority: P2)

**Goal**: Developers can analyze GC performance trends over recent collections to detect regressions

**Independent Test**: Perform multiple collections, query history, verify recent collections are accessible and statistical functions work correctly

### Tests for User Story 5

- [x] T050 [P] [US5] Add unit test `test_gc_history_new` in `crates/rudo-gc/src/metrics.rs`
- [x] T051 [P] [US5] Add integration test `test_history_ring_buffer` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T052 [P] [US5] Add integration test `test_history_wrap_around` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T053 [P] [US5] Add integration test `test_history_average_pause` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T054 [P] [US5] Add integration test `test_history_max_pause` in `crates/rudo-gc/tests/metrics_tests.rs`
- [x] T055 [P] [US5] Add integration test `test_history_empty` in `crates/rudo-gc/tests/metrics_tests.rs`

### Implementation for User Story 5

- [x] T057 [US5] Add `HISTORY_SIZE` constant (64) in `crates/rudo-gc/src/metrics.rs`
- [x] T058 [US5] Add `GcHistory` struct with `UnsafeCell<[GcMetrics; HISTORY_SIZE]>` and `AtomicUsize` write_idx in `crates/rudo-gc/src/metrics.rs`
- [x] T059 [US5] Add `unsafe impl Sync for GcHistory` with SAFETY comment documenting single-writer guarantee in `crates/rudo-gc/src/metrics.rs`
- [x] T060 [US5] Add `GcHistory::new()` const constructor initializing buffer and write_idx in `crates/rudo-gc/src/metrics.rs`
- [x] T061 [US5] Add `GcHistory::push()` internal method writing to buffer and advancing write_idx with `Release` ordering in `crates/rudo-gc/src/metrics.rs`
- [x] T062 [US5] Add `GcHistory::total_recorded()` method loading write_idx with `Acquire` ordering in `crates/rudo-gc/src/metrics.rs`
- [x] T063 [US5] Add `GcHistory::recent(n)` method reading last N entries (newest first) in `crates/rudo-gc/src/metrics.rs`
- [x] T064 [US5] Add `GcHistory::average_pause_time(n)` method computing average from `recent(n)` durations in `crates/rudo-gc/src/metrics.rs`
- [x] T065 [US5] Add `GcHistory::max_pause_time(n)` method computing max from `recent(n)` durations in `crates/rudo-gc/src/metrics.rs`
- [x] T066 [US5] Add static singleton `GC_HISTORY: GcHistory` in `crates/rudo-gc/src/metrics.rs`
- [x] T067 [US5] Add `gc_history()` accessor function returning `&'static GcHistory` in `crates/rudo-gc/src/metrics.rs`
- [x] T068 [US5] Update `record_metrics()` to call `GC_HISTORY.push(metrics)` after global counter updates in `crates/rudo-gc/src/metrics.rs`
- [x] T069 [US5] Update `lib.rs` re-exports to include `gc_history` and `GcHistory` in `crates/rudo-gc/src/lib.rs`

**Checkpoint**: At this point, User Story 5 should be fully functional - GC history provides trend analysis

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, validation, and final integration

- [ ] T070 [P] Add doc comments with examples to all public items in `crates/rudo-gc/src/metrics.rs`
- [ ] T071 [P] Add doc comments with examples to `GlobalMetrics` accessor methods in `crates/rudo-gc/src/metrics.rs`
- [ ] T072 [P] Add doc comments with examples to `GcHistory` methods in `crates/rudo-gc/src/metrics.rs`
- [ ] T073 [P] Add doc comments to heap query functions in `crates/rudo-gc/src/metrics.rs`
- [ ] T074 Run `cargo fmt --all` and verify formatting
- [ ] T075 Run `./clippy.sh` and fix all warnings
- [ ] T076 Run `./test.sh` and verify all tests pass
- [ ] T077 Run `./miri-test.sh` for `GcHistory` UnsafeCell safety verification
- [ ] T078 Verify backward compatibility - existing code using `last_gc_metrics()` still works
- [ ] T079 Verify all re-exports in `lib.rs` are correct and complete
- [ ] T080 Run quickstart.md validation checklist

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3-7)**: All depend on Foundational phase completion
  - User stories can proceed sequentially in priority order (US1 → US2 → US3 → US4 → US5)
  - US1 and US2 share foundational components but can be tested independently
  - US3 and US4 are independent and can be implemented in parallel after US1/US2
  - US5 depends on `GcMetrics` being complete (from US1/US2) but is otherwise independent
- **Polish (Phase 8)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Depends on Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P1)**: Depends on Foundational (Phase 2) - Shares `GcMetrics` extension with US1 but independently testable
- **User Story 3 (P1)**: Depends on Foundational (Phase 2) - Independent, can start after US1/US2 or in parallel
- **User Story 4 (P1)**: Depends on Foundational (Phase 2) - Independent, can start after US1/US2 or in parallel
- **User Story 5 (P2)**: Depends on Foundational (Phase 2) and `GcMetrics` completion (US1/US2) - Requires `GcMetrics` struct to be complete

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Core structs before methods
- Methods before integration
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel
- All Foundational tasks marked [P] can run in parallel (within Phase 2)
- Once Foundational phase completes:
  - US1 and US2 can proceed sequentially (they share `GcMetrics` extension)
  - US3 and US4 can run in parallel after US1/US2
  - US5 can start after US1/US2 complete
- All tests for a user story marked [P] can run in parallel
- Different user stories can be worked on in parallel by different team members (with coordination)

---

## Parallel Example: User Story 3 and User Story 4

```bash
# After US1/US2 complete, US3 and US4 can run in parallel:

# Developer A: User Story 3 (GlobalMetrics)
Task: "Add GlobalMetrics struct with 8 atomic fields"
Task: "Add GlobalMetrics::new() const constructor"
Task: "Add static singleton GLOBAL_METRICS"
Task: "Add global_metrics() accessor function"
Task: "Add 8 read accessor methods"
Task: "Update record_metrics() to increment GLOBAL_METRICS counters"

# Developer B: User Story 4 (Heap Queries)
Task: "Add current_heap_size() function"
Task: "Add current_young_size() function"
Task: "Add current_old_size() function"
Task: "Update lib.rs re-exports"
```

---

## Implementation Strategy

### MVP First (User Stories 1-2 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1 (Phase Timing)
4. Complete Phase 4: User Story 2 (Incremental Stats)
5. **STOP and VALIDATE**: Test User Stories 1-2 independently
6. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (Phase timing MVP!)
3. Add User Story 2 → Test independently → Deploy/Demo (Incremental visibility)
4. Add User Story 3 → Test independently → Deploy/Demo (Cumulative stats)
5. Add User Story 4 → Test independently → Deploy/Demo (Heap queries)
6. Add User Story 5 → Test independently → Deploy/Demo (History trends)
7. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (Phase Timing)
   - Developer B: User Story 2 (Incremental Stats) - after US1 or in parallel with coordination
3. After US1/US2:
   - Developer A: User Story 3 (GlobalMetrics)
   - Developer B: User Story 4 (Heap Queries)
4. After US3/US4:
   - Developer A: User Story 5 (GC History)
5. Stories complete and integrate independently

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- All GC tests must use `--test-threads=1` to avoid interference
- Miri tests required for `GcHistory` UnsafeCell safety
- All public items must have doc comments with examples
- Backward compatibility must be maintained - existing `last_gc_metrics()` callers unaffected
