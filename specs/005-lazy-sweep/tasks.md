# Tasks: Lazy Sweep for Garbage Collection

**Input**: Design documents from `/specs/005-lazy-sweep/`
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Tests are included - write tests first, ensure they FAIL before implementation

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Feature Flag Infrastructure)

**Purpose**: Configure feature flag and basic infrastructure for lazy sweep

- [X] T001 Add `lazy-sweep` feature flag to crates/rudo-gc/Cargo.toml with default enabled
- [X] T002 Update Cargo.toml default features to include `lazy-sweep` and `derive`

---

## Phase 2: Foundational (PageHeader Modifications)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T003 [P] Add PAGE_FLAG_NEEDS_SWEEP constant (0x04) in crates/rudo-gc/src/heap.rs
- [X] T004 [P] Add PAGE_FLAG_ALL_DEAD constant (0x08) in crates/rudo-gc/src/heap.rs
- [X] T005 [P] Add helper methods to PageHeader for sweep flag management in crates/rudo-gc/src/heap.rs:
  - `needs_sweep()`, `set_needs_sweep()`, `clear_needs_sweep()`
  - `all_dead()`, `set_all_dead()`, `clear_all_dead()`
- [X] T006 Replace `_padding: [u8; 2]` with `dead_count: Cell<u16>` in PageHeader struct in crates/rudo-gc/src/heap.rs

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - Eliminate STW Pause During GC Sweep (Priority: P1) 🎯 MVP

**Goal**: Implement core lazy sweep functionality that eliminates stop-the-world pause times by performing sweep incrementally during allocation

**Independent Test**: Run GC-intensive workloads with latency measurements, verifying maximum pause times remain bounded regardless of heap size

### Tests for User Story 1

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [X] T007 [P] [US1] Add test `test_lazy_sweep_frees_dead_objects` in crates/rudo-gc/tests/lazy_sweep.rs
- [X] T008 [P] [US1] Add test `test_lazy_sweep_preserves_live_objects` in crates/rudo-gc/tests/lazy_sweep.rs
- [X] T009 [P] [US1] Add test `test_lazy_sweep_eliminates_stw_pause` in crates/rudo-gc/tests/lazy_sweep.rs

### Implementation for User Story 1

- [X] T010 [P] [US1] Add SWEEP_BATCH_SIZE constant (= 16) in crates/rudo-gc/src/gc/gc.rs
- [X] T011 [P] [US1] Implement `lazy_sweep_page()` function in crates/rudo-gc/src/gc/gc.rs:
  - Processes up to 16 objects per call
  - Handles regular objects (non-large, non-orphan)
  - Reclaims dead objects to free list
  - Clears mark bits for surviving objects
- [X] T012 [US1] Implement `lazy_sweep_page_all_dead()` fast path in crates/rudo-gc/src/gc/gc.rs:
  - Handles pages where all objects are dead
  - Rebuilds free list without individual object scanning
- [X] T013 [US1] Modify mark phase in `perform_multi_threaded_collect()` in crates/rudo-gc/src/gc/gc.rs:
  - Set PAGE_FLAG_NEEDS_SWEEP on pages with allocated objects
  - Do NOT perform STW sweep - return immediately

**Checkpoint**: User Story 1 should be fully functional - lazy sweep eliminates STW pauses

---

## Phase 4: User Story 2 - Memory Reclaimed During Allocation (Priority: P1)

**Goal**: Integrate lazy sweep with allocation path so reclaimed memory is available for new allocations

**Independent Test**: Create workload that allocates/discards objects, verify heap size stabilizes and reclaimed memory is reused

### Tests for User Story 2

- [X] T014 [P] [US2] Add test `test_allocated_memory_reused_after_sweep` in crates/rudo-gc/tests/lazy_sweep.rs
- [X] T015 [P] [US2] Add test `test_heap_size_bounded_under_workload` in crates/rudo-gc/tests/lazy_sweep.rs

### Implementation for User Story 2

- [X] T016 [P] [US2] Add `alloc_from_pending_sweep()` helper method in crates/rudo-gc/src/heap.rs:
  - Scans pages for one needing sweep
  - Calls lazy_sweep_page to reclaim objects
  - Returns allocation from reclaimed free list if available
- [X] T017 [US2] Modify `alloc<T>()` method in crates/rudo-gc/src/heap.rs:
  - Add lazy sweep attempt before alloc_slow (new page)
  - Call alloc_from_pending_sweep with proper size class matching

**Checkpoint**: User Story 2 complete - memory reclaimed during allocation, heap bounded

---

## Phase 5: User Story 3 - Lazy Sweep Behavior Defaults (Priority: P2)

**Goal**: Ensure lazy sweep is enabled by default, with eager sweep available when disabled

**Independent Test**: Build rudo-gc with default features, verify lazy sweep active; build without lazy-sweep, verify eager sweep

### Tests for User Story 3

- [X] T018 [P] [US3] Add test `test_lazy_sweep_enabled_by_default` in crates/rudo-gc/tests/lazy_sweep.rs
- [X] T019 [P] [US3] Add test `test_eager_sweep_when_feature_disabled` in crates/rudo-gc/tests/lazy_sweep.rs

### Implementation for User Story 3

- [X] T020 [P] [US3] Add cfg attributes to existing sweep code in crates/rudo-gc/src/gc/gc.rs:
  - Lazy sweep: modify mark phase to skip STW sweep
  - Eager sweep (feature disabled): keep existing STW sweep behavior
- [X] T021 [P] [US3] Add cfg attributes to check_safepoint() in crates/rudo-gc/src/heap.rs:
  - Lazy sweep: include lazy sweep trigger
  - Eager sweep: no changes needed

**Checkpoint**: User Story 3 complete - lazy sweep is default behavior

---

## Phase 6: User Story 4 - Large Objects Use Eager Sweep (Priority: P2)

**Goal**: Ensure large objects are reclaimed promptly using eager sweep, not lazy sweep

**Independent Test**: Allocate large objects, discard them, verify reclaimed promptly (not delayed)

### Tests for User Story 4

- [X] T022 [P] [US4] Add test `test_large_object_still_eager` in crates/rudo-gc/tests/lazy_sweep.rs
- [X] T023 [P] [US4] Add test `test_orphan_page_still_eager` in crates/rudo-gc/tests/lazy_sweep.rs

### Implementation for User Story 4

- [X] T024 [P] [US4] Modify lazy_sweep_page() in crates/rudo-gc/src/gc/gc.rs:
  - Skip pages with PAGE_FLAG_LARGE set (return false, don't sweep)
- [X] T025 [US4] Ensure large object pages are swept eagerly during mark phase completion:
  - In perform_multi_threaded_collect(), sweep large object pages immediately
- [X] T026 [US4] Ensure orphan pages are swept eagerly:
  - Orphan pages continue using existing eager sweep behavior

**Checkpoint**: User Story 4 complete - large objects and orphans use eager sweep

---

## Phase 7: User Story 5 - Weak References Handled Correctly (Priority: P2)

**Goal**: Preserve weak reference semantics during lazy sweep

**Independent Test**: Create weak references, drop strong refs, verify weak refs report dead status correctly

### Tests for User Story 5

- [X] T027 [P] [US5] Add test `test_lazy_sweep_weak_refs` in crates/rudo-gc/tests/lazy_sweep.rs
- [X] T028 [P] [US5] Add test `test_weak_ref_value_dropped_but_allocation_preserved` in crates/rudo-gc/tests/lazy_sweep.rs

### Implementation for User Story 5

- [X] T029 [P] [US5] Modify lazy_sweep_page() in crates/rudo-gc/src/gc/gc.rs:
  - Check weak_count() on each dead object
  - If weak_count > 0: only drop value, keep allocation (set dead flag)
  - If weak_count == 0: fully reclaim to free list
- [X] T030 [US5] Ensure GcBox::weak_count() and related methods are available for lazy sweep check

**Checkpoint**: User Story 5 complete - weak references work correctly with lazy sweep

---

## Phase 8: User Story 6 - Public API for Sweep Control (Priority: P3)

**Goal**: Provide programmatic access to sweep operations for advanced use cases and testing

**Independent Test**: Call public API functions, verify they return correct information about pending sweep work

### Tests for User Story 6

- [X] T031 [P] [US6] Add test `test_sweep_pending_returns_correct_count` in crates/rudo-gc/tests/lazy_sweep.rs
- [X] T032 [P] [US6] Add test `test_pending_sweep_pages_returns_accurate_count` in crates/rudo-gc/tests/lazy_sweep.rs

### Implementation for User Story 6

- [X] T033 [P] [US6] Implement `sweep_pending()` function in crates/rudo-gc/src/gc/gc.rs:
  - Sweeps up to specified number of pages
  - Returns count of pages actually swept
- [X] T034 [P] [US6] Implement `pending_sweep_count()` function in crates/rudo-gc/src/gc/gc.rs:
  - Counts pages with PAGE_FLAG_NEEDS_SWEEP set
- [X] T035 [US6] Export public API functions in crates/rudo-gc/src/lib.rs:
  - `sweep_pending(num_pages: usize) -> usize`
  - `pending_sweep_pages() -> usize`
  - Add cfg(feature = "lazy-sweep") conditional compilation

**Checkpoint**: User Story 6 complete - public API available for sweep control

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Performance validation, benchmarks, and documentation

- [X] T036 [P] Add benchmarks in crates/rudo-gc/tests/benchmarks/sweep_comparison.rs:
  - bench_sweep_eager_pause_time
  - bench_sweep_lazy_pause_time
  - bench_sweep_eager_throughput
  - bench_sweep_lazy_throughput
- [X] T037 [P] Run `./test.sh` to verify all tests pass including lazy_sweep tests
- [X] T038 Run `./clippy.sh` to verify zero warnings
- [X] T039 Run `./miri-test.sh` to verify unsafe code passes memory safety checks
- [X] T040 [P] Add SAFETY comments to all unsafe blocks in lazy sweep implementation
- [X] T041 Verify all acceptance scenarios from spec.md are tested

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3-8)**: All depend on Foundational phase completion
  - User stories can proceed in parallel after Phase 2
  - US1 and US2 (both P1) are most critical for MVP
- **Polish (Phase 9)**: Depends on all desired user stories being complete

### User Story Dependencies

| Story | Priority | Dependencies |
|-------|----------|--------------|
| US1 | P1 | Phase 2 complete - No other story dependencies |
| US2 | P1 | Phase 2 complete - Builds on US1 infrastructure |
| US3 | P2 | Phase 2 complete - Can work in parallel with US1/US2 |
| US4 | P2 | Phase 2 complete - Can work in parallel |
| US5 | P2 | Phase 2 complete - Can work in parallel |
| US6 | P3 | Phase 2 complete - Last priority |

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Core functions (lazy_sweep_page) before integration (alloc path)
- Story complete before moving to polish phase

### Parallel Opportunities

- All Setup tasks (T001, T002) can run in parallel
- All Foundational tasks (T003-T006) can run in parallel
- Once Foundational is done:
  - US1 tests (T007-T009) can be written in parallel
  - US2 tests (T014-T015) can be written in parallel
  - US1 implementation (T010-T013) can proceed
  - US2 implementation (T016-T017) can proceed once T010-T013 complete
- US3-US6 can proceed in parallel with each other and with later US1/US2 work

---

## Parallel Execution Examples

### Example 1: Foundational Phase (T003-T006)
```bash
Task: "Add PAGE_FLAG_NEEDS_SWEEP constant in heap.rs"
Task: "Add PAGE_FLAG_ALL_DEAD constant in heap.rs"
Task: "Add helper methods for sweep flags in heap.rs"
Task: "Replace padding with dead_count in PageHeader"
```

### Example 2: User Story 1 Tests (T007-T009)
```bash
Task: "Add test_lazy_sweep_frees_dead_objects in tests/lazy_sweep.rs"
Task: "Add test_lazy_sweep_preserves_live_objects in tests/lazy_sweep.rs"
Task: "Add test_lazy_sweep_eliminates_stw_pause in tests/lazy_sweep.rs"
```

### Example 3: User Story 1 Implementation (T010-T011)
```bash
Task: "Add SWEEP_BATCH_SIZE constant in gc.rs"
Task: "Implement lazy_sweep_page function in gc.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 + User Story 2)

1. Complete Phase 1: Setup (T001-T002)
2. Complete Phase 2: Foundational (T003-T006)
3. Complete Phase 3: User Story 1 (T007-T013)
4. Complete Phase 4: User Story 2 (T014-T017)
5. **STOP and VALIDATE**: Run latency tests, verify STW pause elimination
6. Deploy/demo if ready - core lazy sweep is functional

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add US1 + US2 → Test independently → Deploy/Demo (MVP!)
3. Add US3 (feature defaults) → Test → Deploy/Demo
4. Add US4 (large objects) + US5 (weak refs) → Test → Deploy/Demo
5. Add US6 (public API) → Test → Deploy/Demo
6. Add Phase 9 (benchmarks, polish) → Final release

### Parallel Team Strategy

With multiple developers:

1. Developer A: Complete Setup + Foundational (Phase 1-2)
2. Once Foundational done:
   - Developer A: User Story 1 (Phase 3)
   - Developer B: User Story 2 (Phase 4)
   - Developer C: User Story 3-6 (Phase 5-8) in parallel
3. All stories complete → Phase 9 polish together

---

## Task Summary

| Phase | Task Count | Description |
|-------|------------|-------------|
| Phase 1: Setup | 2 | Feature flag configuration |
| Phase 2: Foundational | 4 | PageHeader modifications |
| Phase 3: US1 | 7 | Core lazy sweep implementation |
| Phase 4: US2 | 4 | Allocation path integration |
| Phase 5: US3 | 4 | Feature defaults |
| Phase 6: US4 | 3 | Large object handling |
| Phase 7: US5 | 4 | Weak reference handling |
| Phase 8: US6 | 5 | Public API |
| Phase 9: Polish | 6 | Benchmarks, tests, lint, Miri |
| **Total** | **41** | |

**Parallel-capable tasks**: 23 (marked with [P])
**Sequential/blocking tasks**: 18

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Write tests first, verify they fail before implementation
- Run `./clippy.sh` and `./miri-test.sh` after implementation
- Stop at any checkpoint to validate story independently
- US1 + US2 together form the complete MVP for lazy sweep
