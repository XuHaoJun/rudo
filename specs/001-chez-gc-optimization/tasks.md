# Tasks: Chez Scheme GC optimizations for rudo-gc

**Input**: Design documents from `/specs/001-chez-gc-optimization/`
**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/
**Tests**: Integration tests and benchmarks included per feature specification

**Organization**: Tasks grouped by user story to enable independent implementation and testing

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story (US1, US2, US3, US4)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and lock ordering documentation

- [X] T001 Create lock ordering documentation in `crates/rudo-gc/src/gc/sync.rs` defining LocalHeap -> GlobalMarkState -> GC Request order
- [X] T002 Add lock order validation infrastructure with `AtomicU8` tags in `crates/rudo-gc/src/gc/sync.rs` (debug builds only)
- [X] T003 Create `crates/rudo-gc/src/gc/mark/` directory structure for new mark-related modules
- [X] T004 [P] Create `crates/rudo-gc/src/gc/mark/bitmap.rs` empty module file for MarkBitmap implementation
- [X] T005 [P] Create `crates/rudo-gc/src/gc/mark/ownership.rs` empty module file for ownership integration
- [X] T006 Verify project builds with `./clippy.sh` and `cargo fmt --all`

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure required before any user story; lock ordering MUST be complete first

**⚠️ CRITICAL**: No user story work can begin until lock ordering discipline is enforced

- [X] T007 Implement `LockOrderingDiscipline` constants and validation macros in `crates/rudo-gc/src/gc/sync.rs`
- [X] T008 Add `LOCK_ORDER_*` constants for LocalHeap (1), GlobalMarkState (2), GC Request (3)
- [X] T009 Add `acquire_lock()` function with debug assertions for order validation
- [X] T010 Integrate lock ordering checks into existing `LocalHeap`, `GlobalMarkState`, `GCRequest` lock acquisitions
- [X] T011 Run `./miri-test.sh` to verify no memory safety issues with new lock ordering code
- [X] T012 Write integration test for lock ordering in `tests/integration/lock_ordering.rs`

**Checkpoint**: Lock ordering discipline enforced - all concurrent operations now have deadlock prevention

---

## Phase 3: User Story 2 - Prevention of deadlock and race conditions (Priority: P1) 🎯 MVP

**Goal**: Systematic lock ordering discipline is documented, enforced, and prevents deadlocks

**Independent Test**: Run `./test.sh` with randomized concurrent workloads for 24 hours; no deadlocks occur

**Tests for User Story 2** (write first, verify fail):

- [ ] T013 [P] [US2] Unit test for lock order constants in `tests/unit/test_lock_ordering.rs`
- [ ] T014 [P] [US2] Integration test for concurrent lock acquisition in `tests/integration/lock_ordering.rs`
- [ ] T015 [P] [US2] Stress test for lock ordering under contention in `tests/integration/lock_ordering.rs`

**Implementation for User Story 2**:

- [ ] T016 [US2] Update `LocalHeap` lock acquisition to use `LOCK_ORDER_LOCAL_HEAP` in `src/heap/sync.rs`
- [ ] T017 [US2] Update `GlobalMarkState` lock acquisition to use `LOCK_ORDER_GLOBAL_MARK` in `src/heap/marker.rs`
- [ ] T018 [US2] Update `GCRequest` lock acquisition to use `LOCK_ORDER_GC_REQUEST` in `src/gc.rs`
- [ ] T019 [US2] Add SAFETY comments to all lock acquisition points documenting the ordering contract
- [ ] T020 [US2] Add `#[cfg(debug_assertions)]` runtime validation for lock order in `src/heap/sync.rs`

**Checkpoint**: Lock ordering fully enforced; User Story 2 complete and independently testable

---

## Phase 4: User Story 1 - Reduced GC pause times (Priority: P1)

**Goal**: Push-based work transfer and segment ownership reduce GC pause times by 30%

**Independent Test**: Run concurrent allocation workloads across 4+ threads; 95th percentile pause times reduced by 30%

**Tests for User Story 1** (write first, verify fail):

- [ ] T021 [P] [US1] Unit test for `PerThreadMarkQueue` in `tests/unit/test_mark_queue.rs`
- [ ] T022 [P] [US1] Integration test for push-based transfer in `tests/integration/work_stealing.rs`
- [ ] T023 [P] [US1] Integration test for segment ownership in `tests/integration/parallel_marking.rs`
- [ ] T024 [P] [US1] Benchmark test for GC pause times in `tests/benchmarks/marking.rs`

**Implementation for User Story 1**:

- [ ] T025 [US1] Add `pending_work: Mutex<Vec<MarkWork>>` field to `PerThreadMarkQueue` in `src/heap/mark/queue.rs`
- [ ] T026 [US1] Add `work_available: Notify` notification mechanism to `PerThreadMarkQueue` in `src/heap/mark/queue.rs`
- [ ] T027 [US1] Implement `push_remote()` method in `src/heap/mark/queue.rs`
- [ ] T028 [US1] Implement `receive_work()` method in `src/heap/mark/queue.rs`
- [ ] T029 [US1] Modify `try_steal_work()` to check `pending_work` before stealing in `src/heap/mark/queue.rs`
- [ ] T030 [US1] Implement `add_owned_page()` and `remove_owned_page()` in `src/heap/mark/ownership.rs`
- [ ] T031 [US1] Add `owned_pages: HashSet<PagePtr>` to `PerThreadMarkQueue` in `src/heap/mark/queue.rs`
- [ ] T032 [US1] Implement `get_owned_queues()` method in `src/heap/mark/ownership.rs`
- [ ] T033 [US1] Modify marker to push remote references to owner's queue in `src/marker.rs`
- [ ] T034 [US1] Update `GlobalMarkState` to coordinate ownership-based work distribution in `src/marker.rs`
- [ ] T035 [US1] Integrate with existing work-stealing deque in `src/worklist.rs`

**Checkpoint**: Push-based transfer and ownership implemented; pause time benchmarks should show improvement

---

## Phase 5: User Story 3 - Memory efficiency (Priority: P2)

**Goal**: Mark bitmap replaces forwarding pointers; per-object overhead reduced by 50% for small objects

**Independent Test**: Allocate 10,000 small objects; measure per-object memory overhead; verify 50% reduction

**Tests for User Story 3** (write first, verify fail):

- [ ] T036 [P] [US3] Unit test for `MarkBitmap` in `tests/unit/test_mark_bitmap.rs`
- [ ] T037 [P] [US3] Integration test for bitmap marking in `tests/integration/mark_bitmap.rs`
- [ ] T038 [P] [US3] Migration test from forwarding pointers to bitmap in `tests/integration/mark_bitmap.rs`
- [ ] T039 [P] [US3] Memory overhead benchmark in `tests/benchmarks/marking.rs`

**Implementation for User Story 3**:

- [ ] T040 [US3] Create `MarkBitmap` struct with `Vec<u64>` bitmap storage in `src/heap/mark/bitmap.rs`
- [ ] T041 [US3] Implement `new()` constructor with capacity validation in `src/heap/mark/bitmap.rs`
- [ ] T042 [US3] Implement `mark()` method using word/bit index calculations in `src/heap/mark/bitmap.rs`
- [ ] T043 [US3] Implement `is_marked()` method in `src/heap/mark/bitmap.rs`
- [ ] T044 [US3] Implement `clear()` method for sweep phase in `src/heap/mark/bitmap.rs`
- [ ] T045 [US3] Add `marked_count: AtomicUsize` field and update on mark in `src/heap/mark/bitmap.rs`
- [ ] T046 [US3] Add `bitmap: Option<MarkBitmap>` field to `PageHeader` in `src/heap/page.rs`
- [ ] T047 [US3] Remove `forwarding: GcHeader` field from `GcBox<T>` in `src/gc.rs`
- [ ] T048 [US3] Update mark phase to set bitmap bits instead of forwarding pointers in `src/marker.rs`
- [ ] T049 [US3] Update sweep phase to read bitmap for liveness in `src/gc.rs`
- [ ] T050 [US3] Add SAFETY comments to all unsafe bitmap operations in `src/heap/mark/bitmap.rs`

**Checkpoint**: Mark bitmap implemented; memory overhead benchmarks should show 50% reduction

---

## Phase 6: User Story 4 - Predictable performance (Priority: P2)

**Goal**: Dynamic stack growth monitoring prevents stalls; performance scales with worker count

**Independent Test**: Run benchmarks with 2, 4, 8, 16 workers; throughput scales proportionally without regression

**Tests for User Story 4** (write first, verify fail):

- [ ] T051 [P] [US4] Unit test for dynamic stack growth in `tests/unit/test_mark_queue.rs`
- [ ] T052 [P] [US4] Integration test for queue capacity handling in `tests/integration/work_stealing.rs`
- [ ] T053 [P] [US4] Scalability benchmark with varying worker counts in `tests/benchmarks/marking.rs`

**Implementation for User Story 4**:

- [ ] T054 [US4] Add `capacity_hint: AtomicUsize` to `PerThreadMarkQueue` in `src/heap/mark/queue.rs`
- [ ] T055 [US4] Implement queue capacity monitoring in `push_local()` in `src/heap/mark/queue.rs`
- [ ] T056 [US4] Implement `handle_overflow()` method for capacity growth in `src/heap/mark/queue.rs`
- [ ] T057 [US4] Implement pre-allocation strategy when threshold exceeded in `src/heap/mark/queue.rs`
- [ ] T058 [US4] Add overflow work transfer to remote `pending_work` as fallback in `src/heap/mark/queue.rs`
- [ ] T059 [US4] Add queue capacity utilization metrics in `src/heap/mark/queue.rs`
- [ ] T060 [US4] Integrate dynamic growth with existing Chase-Lev deque in `src/worklist.rs`

**Checkpoint**: Dynamic stack growth implemented; scalability benchmarks should show proportional scaling

---

## Phase 7: Integration & Polish

**Purpose**: Cross-cutting improvements affecting all user stories

- [ ] T061 [P] Update `src/heap/mark/mod.rs` to export all new modules
- [ ] T062 [P] Add module documentation comments to `src/heap/mark/bitmap.rs`, `src/heap/mark/ownership.rs`
- [ ] T063 [P] Update `AGENTS.md` with new concurrency patterns if needed
- [ ] T064 Run full `./test.sh` including all integration tests
- [ ] T065 Run `./miri-test.sh` to verify all unsafe code is memory-safe
- [ ] T066 Run `./clippy.sh` and fix any warnings
- [ ] T067 Run `cargo fmt --all` for consistent formatting
- [ ] T068 Run benchmarks in `tests/benchmarks/marking.rs` to validate performance targets
- [ ] T069 Update `quickstart.md` with any new implementation details
- [ ] T070 [P] Add final integration test combining all optimizations in `tests/integration/full_optimization.rs`

---

## Dependencies & Execution Order

### Phase Dependencies

| Phase | Dependencies | Blocks |
|-------|--------------|--------|
| Phase 1: Setup | None | Phase 2 |
| Phase 2: Foundational | Phase 1 | Phases 3-6 |
| Phase 3: US2 | Phase 2 | Phase 7 |
| Phase 4: US1 | Phase 2 | Phase 7 |
| Phase 5: US3 | Phase 2 | Phase 7 |
| Phase 6: US4 | Phase 2 | Phase 7 |
| Phase 7: Polish | Phases 3-6 | Done |

### User Story Dependencies

| Story | Depends On | Can Run After |
|-------|------------|---------------|
| US2: Lock Ordering | Phase 2 | Phase 2 complete |
| US1: Push-based + Ownership | Lock ordering (US2) | Phase 2 complete |
| US3: Mark Bitmap | Lock ordering (US2) | Phase 2 complete |
| US4: Dynamic Stack | Queue changes (US1) | Phase 4 complete |

### Recommended Execution Order

1. **Sequential**: Phase 1 → Phase 2 → Phase 3 → Phase 4 → Phase 5 → Phase 6 → Phase 7
2. **Parallel (after Foundational)**: 
   - US2 (Lock Ordering) - CRITICAL, must be first
   - US1 and US3 can run in parallel after US2
   - US4 runs after US1 (depends on queue changes)

---

## Parallel Opportunities

After Phase 2 (Foundational) completes:

| Tasks | Can Run In Parallel Because |
|-------|----------------------------|
| T013-T015 (US2 tests) | Different test files |
| T021-T024 (US1 tests) | Different test files |
| T036-T039 (US3 tests) | Different test files |
| T040-T050 (US3 impl) | Multiple files in bitmap module |
| T051-T060 (US4 impl) | Multiple files in queue module |
| T061-T063, T069 (docs) | Documentation updates, no dependencies |

---

## Parallel Example: After Phase 2

```bash
# Developer A: Complete US2 (lock ordering)
Task: T013 - Unit test for lock ordering
Task: T016 - Update LocalHeap lock acquisition
Task: T017 - Update GlobalMarkState lock acquisition
...

# Developer B: Start US1 (push-based transfer) in parallel
Task: T021 - Unit test for PerThreadMarkQueue
Task: T025 - Add pending_work field
Task: T026 - Add work_available notification
...

# Developer C: Start US3 (mark bitmap) in parallel
Task: T036 - Unit test for MarkBitmap
Task: T040 - Create MarkBitmap struct
Task: T041 - Implement new() constructor
...
```

---

## Implementation Strategy

### MVP First (User Story 2 only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (lock ordering)
3. Complete Phase 3: User Story 2 (lock ordering enforcement)
4. **STOP and VALIDATE**: Test deadlock prevention independently
5. Deploy/demo lock ordering as foundational improvement

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add US2 → Deploy (lock ordering prevents deadlocks)
3. Add US1 → Deploy (reduced GC pause times)
4. Add US3 → Deploy (memory efficiency improvement)
5. Add US4 → Deploy (predictable performance)
6. Each optimization adds value without breaking previous work

### Full Feature Delivery

1. Complete all phases sequentially
2. Run full benchmark suite
3. Validate all success criteria:
   - SC-001: 30% reduction in p95 pause time
   - SC-002: 90% steal success without retry
   - SC-003: 50% memory overhead reduction
   - SC-004: Zero deadlocks in 24-hour test
   - SC-005: Zero lock ordering violations
   - SC-006: Linear scaling to 16 threads

---

## Task Summary

| Category | Count |
|----------|-------|
| Setup tasks | 6 |
| Foundational tasks | 6 |
| User Story 2 (Lock Ordering) | 9 |
| User Story 1 (Push-based + Ownership) | 14 |
| User Story 3 (Mark Bitmap) | 12 |
| User Story 4 (Dynamic Stack) | 10 |
| Integration & Polish | 10 |
| **Total Tasks** | **67** |

---

## Validation Checklist

Before marking tasks complete:

- [ ] All code passes `./clippy.sh`
- [ ] All code formatted with `cargo fmt --all`
- [ ] All tests pass `./test.sh --include-ignored`
- [ ] All unsafe code passes `./miri-test.sh`
- [ ] Benchmarks show performance improvements per success criteria
- [ ] Documentation updated in `quickstart.md`
- [ ] SAFETY comments added to all unsafe operations
