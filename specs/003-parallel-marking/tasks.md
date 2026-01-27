# Tasks: Parallel Marking for rudo-gc

**Feature**: Parallel Marking  
**Feature Branch**: `003-parallel-marking`  
**Generated**: 2026-01-27  
**Input Spec**: [spec.md](spec.md), [plan.md](plan.md), [research.md](research.md), [data-model.md](data-model.md), [contracts/api.md](contracts/api.md)

---

## Summary

| Metric | Value |
|--------|-------|
| Total Tasks | 24 |
| User Stories | 5 (2 P1, 3 P2) |
| Parallelizable Tasks | 8 |
| Estimated Duration | 4 weeks |

---

## Dependencies Graph

```
Phase 1 (Setup)
    ↓
Phase 2 (Foundational)
    ↓
Phase 3: US1 (Parallel Marking Core)
    ↓
Phase 4: US2 (Minor GC Parallel) ──┐
                                   ↓
Phase 5: US3 (Work Stealing) ──────┼──→ Phase 7: Polish & Cross-Cutting
                                   ↓
Phase 6: US4 (Cross-Thread Refs) ──┘
```

**Legend**: → = blocking dependency, ──→ = can be done after but not blocked

---

## Phase 1: Setup

**Goal**: Initialize project structure and verify build environment.

### Tasks

- [X] T001 Create `crates/rudo-gc/src/gc/worklist.rs` module file with basic module declaration in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [X] T002 Create `crates/rudo-gc/src/gc/marker.rs` module file with basic module declaration in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [X] T003 Add module exports to `crates/rudo-gc/src/gc.rs` for `worklist` and `marker` modules in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc.rs`
- [X] T004 Create test file `crates/rudo-gc/tests/parallel_gc.rs` with basic test module structure in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`
- [X] T005 Run `./clippy.sh` to verify no lint warnings in project root
- [X] T006 Run `./test.sh` to verify all existing tests pass in project root

---

## Phase 2: Foundational Data Structures

**Goal**: Implement core lock-free data structures required by all user stories.

### Tasks

- [X] T007 [P] Implement `StealQueue<T, const N: usize>` struct with buffer, bottom, top, mask fields in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [X] T008 [P] Implement `StealQueue::new()` constructor with power-of-2 validation in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [X] T009 [P] Implement `StealQueue::push()` LIFO push operation with SAFETY comments in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [X] T010 [P] Implement `StealQueue::pop()` LIFO pop operation with SAFETY comments in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [X] T011 [P] Implement `StealQueue::steal()` FIFO steal operation with SAFETY comments in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [X] T012 [P] Implement `StealQueue::len()`, `is_empty()`, `is_full()` helper methods in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [X] T013 Add unit tests for `StealQueue` operations in `crates/rudo-gc/src/gc/worklist.rs`
- [X] T014 [P] Add `owner_thread: std::thread::ThreadId` field to `PageHeader` struct in `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs`
- [X] T015 [P] Implement `PageHeader::try_mark()` CAS-based atomic marking with SAFETY comments in `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs`
- [X] T016 [P] Implement `PageHeader::is_fully_marked()` method to check all objects marked in `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs`

---

## Phase 3: User Story 1 - Multi-threaded GC Performance Improvement

**Priority**: P1  
**Goal**: Implement parallel Major GC marking with configurable worker count.  
**Independent Test**: Run multi-threaded app with 100,000+ objects, measure marking phase duration with different worker counts.  
**Acceptance**: Marking completes in time proportional to (work / workers).

### Tasks

- [X] T017 [P] [US1] Implement `PerThreadMarkQueue` struct with local_queue, steal_queue, owned_pages, marked_count fields in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [X] T018 [US1] Implement `PerThreadMarkQueue::new()` constructor in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [X] T019 [US1] Implement `PerThreadMarkQueue::push_local()` and `pop_local()` methods in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [X] T020 [US1] Implement `PerThreadMarkQueue::steal()` method (called by other threads) in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T021 [US1] Implement `PerThreadMarkQueue::process_owned_page()` to process all objects on owned page in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [X] T022 [P] [US1] Implement `ParallelMarkCoordinator` struct with queues, barrier, page_to_queue, total_marked fields in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [X] T023 [US1] Implement `ParallelMarkCoordinator::new()` with worker count validation in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T024 [US1] Implement `ParallelMarkCoordinator::register_pages()` to map page addresses to queue indices in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T025 [US1] Implement `ParallelMarkCoordinator::distribute_roots()` to assign roots to appropriate workers in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T026 [US1] Implement `ParallelMarkCoordinator::mark()` with barrier synchronization in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [X] T027 [US1] Integrate parallel marking into `perform_multi_threaded_collect()` in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc.rs`
- [X] T028 [US1] Add `GcVisitorConcurrent` visitor for parallel marking with reference routing in `/home/noah/Desktop/rudo/crates/rudo-gc/src/trace.rs`
- [X] T029 [US1] Add integration test `test_parallel_major_gc()` for correctness in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`
- [X] T030 [US1] Add benchmark test `benchmark_parallel_marking_performance()` to verify speedup in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`

---

## Phase 4: User Story 2 - Minor GC with Parallel Marking

**Priority**: P1  
**Goal**: Implement parallel Minor GC marking for old->young references via dirty bits.  
**Independent Test**: Create old gen objects referencing young objects, repeatedly allocate/drop young objects, measure Minor GC marking time.  
**Acceptance**: Marking processes dirty pages in parallel across multiple threads.

### Tasks

- [ ] T031 [P] [US2] Implement `ParallelMarkCoordinator::distribute_dirty_pages()` for Minor GC in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T032 [US2] Modify `mark_minor_roots_multi()` to use parallel marking coordinator in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc.rs`
- [ ] T033 [US2] Add integration test `test_parallel_minor_gc()` for dirty page processing in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`
- [ ] T034 [US2] Add test `test_multi_thread_local_heap_dirty_pages()` to verify each thread's dirty pages processed correctly in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`

---

## Phase 5: User Story 3 - Work Stealing Load Balancing

**Priority**: P2  
**Goal**: Implement work stealing to balance load when work distribution is uneven.  
**Independent Test**: Create allocation patterns where one thread allocates 10x more objects, verify all work completed without significant stragglers.  
**Acceptance**: Total marking time dominated by largest queue, not sum of all queues.

### Tasks

- [ ] T035 [P] [US3] Implement work stealing algorithm in `PerThreadMarkQueue` with steal attempt loop in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T036 [US3] Implement `try_steal()` helper that iterates through other queues in FIFO order in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T037 [US3] Add test `test_work_stealing_balance()` with uneven allocation patterns in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`
- [ ] T038 [US3] Add test `test_steal_from_other_queues()` verifying successful steal operations in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`

---

## Phase 6: User Story 4 - Cross-Thread Object References

**Priority**: P2  
**Goal**: Correctly mark objects reachable across thread boundaries.  
**Independent Test**: Create object graph distributed across multiple threads with cross-references, trigger GC, verify all reachable objects retained.  
**Acceptance**: Objects in different threads' heaps are correctly marked when referenced.

### Tasks

- [ ] T039 [US4] Implement `GcVisitorConcurrent::add_ref()` to route references to owning worker's queue in `/home/noah/Desktop/rudo/crates/rudo-gc/src/trace.rs`
- [ ] T040 [US4] Implement HashMap lookup for `page_to_queue` routing in parallel marking coordinator in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T041 [US4] Add test `test_cross_thread_references()` with objects spanning multiple threads in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`
- [ ] T042 [US4] Add test `test_three_thread_object_chain()` verifying A->B->C reference chain marking in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`

---

## Phase 7: Polish & Cross-Cutting Concerns

**Goal**: Finalize implementation, add safety tests, verify quality gates.

### Tasks

- [ ] T043 [P] Add `ParallelMarkConfig` struct with max_workers, queue_capacity, parallel_minor_gc, parallel_major_gc fields in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T044 Implement single-threaded fallback when fewer than 2 workers available in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T045 Add Miri test `test_marking_completeness_miri()` for marking correctness verification in `/home/noah/Desktop/rudo/crates/rudo-gc/tests/parallel_gc.rs`
- [ ] T046 Add Miri test for `StealQueue` operations to verify lock-free correctness in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/worklist.rs`
- [ ] T047 Add SAFETY comments to all remaining unsafe blocks in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/marker.rs`
- [ ] T048 Run `./clippy.sh` and fix all warnings in project root
- [ ] T049 Run `./test.sh` with `--test-threads=1` to verify all tests pass in project root
- [ ] T050 Run `./miri-test.sh` to verify all unsafe code passes Miri validation in project root
- [ ] T051 Run `cargo fmt --all` to format all code consistently in project root

---

## Parallel Execution Opportunities

### Tasks that can run in parallel (no dependencies)

| Group | Tasks | Reason |
|-------|-------|--------|
| Group A | T007-T012 | Different methods on StealQueue, independent |
| Group B | T014-T016 | Different PageHeader additions, independent |
| Group C | T017-T021 | PerThreadMarkQueue methods, build on struct |
| Group D | T031, T035 | US2 and US3 features, independent |
| Group E | T039-T042 | Cross-thread tests, can be added anytime |
| Group F | T043, T044 | Config and fallback, independent features |

### Suggested parallelization

- **Week 1**: T001-T006 (Setup) → T007-T016 (Foundational, Group A+B)
- **Week 2**: T017-T026 (US1 Core, Group C) → T027-T030 (US1 Integration)
- **Week 3**: T031-T034 (US2) + T035-T038 (US3) can run in parallel
- **Week 4**: T039-T042 (US4) → T043-T051 (Polish)

---

## Implementation Strategy

### MVP Scope (User Story 1 only)

For minimum viable product, implement only:
- T001-T016: Foundational data structures
- T017-T027: US1 Core (PerThreadMarkQueue + ParallelMarkCoordinator)
- T028: Integration into gc.rs
- T029: Basic integration test

This enables parallel Major GC marking with 4-8 workers, achieving 50-65% time reduction on multi-core systems.

### Incremental Delivery

1. **Iteration 1**: StealQueue + PageHeader additions (T007-T016)
2. **Iteration 2**: PerThreadMarkQueue (T017-T021)
3. **Iteration 3**: ParallelMarkCoordinator (T022-T026)
4. **Iteration 4**: GC integration (T027-T030)
5. **Iteration 5**: Minor GC parallel (T031-T034)
6. **Iteration 6**: Work stealing (T035-T038)
7. **Iteration 7**: Cross-thread references (T039-T042)
8. **Iteration 8**: Polish and testing (T043-T051)

---

## Independent Test Criteria

### User Story 1 Tests
- `test_parallel_major_gc()`: Multi-threaded Major GC correctness
- `benchmark_parallel_marking_performance()`: Speedup verification

### User Story 2 Tests
- `test_parallel_minor_gc()`: Dirty page parallel processing
- `test_multi_thread_local_heap_dirty_pages()`: Per-thread dirty page handling

### User Story 3 Tests
- `test_work_stealing_balance()`: Load balancing with uneven allocation
- `test_steal_from_other_queues()`: Steal operation verification

### User Story 4 Tests
- `test_cross_thread_references()`: Cross-heap reference marking
- `test_three_thread_object_chain()`: Multi-thread reference chain

### Safety Tests
- Miri tests for StealQueue operations
- Miri tests for try_mark() correctness

---

## Quality Gates (per AGENTS.md)

All tasks must pass before merge:
1. `./clippy.sh` - Zero warnings
2. `cargo fmt --all` - No changes needed
3. `./test.sh` - All tests pass (including ignored)
4. `./miri-test.sh` - All unsafe code passes

---

## Notes

- All unsafe code MUST have explicit SAFETY comments
- Use `--test-threads=1` for all GC interference tests
- Follow existing code style in `crates/rudo-gc/src/`
- Mark tasks as complete with checkbox when done
