# Tasks: Generational GC Dirty Page Tracking

**Input**: Design documents from `/specs/007-gen-gc-dirty-pages/`  
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/, quickstart.md  

**Tests**: Included (unit, integration, loom) per success criteria and spec.  

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3, US4)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and dependency setup

- [ ] T001 Add parking_lot dependency in `crates/rudo-gc/Cargo.toml`

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [ ] T002 Add `PAGE_FLAG_DIRTY_LISTED` constant in `crates/rudo-gc/src/heap.rs`
- [ ] T003 Add PageHeader dirty-listed helpers in `crates/rudo-gc/src/heap.rs`
- [ ] T004 Add dirty page fields to LocalHeap in `crates/rudo-gc/src/heap.rs`
- [ ] T005 Add LocalHeap dirty page methods in `crates/rudo-gc/src/heap.rs`
- [ ] T006 Initialize dirty page fields in `LocalHeap::new()` in `crates/rudo-gc/src/heap.rs`
- [ ] T007 Update write_barrier to add dirty pages (small/large paths) in `crates/rudo-gc/src/cell.rs`

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - Reduced Minor GC Pause Times (Priority: P1) 🎯 MVP

**Goal**: Replace O(num_pages) minor GC scanning with O(dirty_pages) scanning

**Independent Test**: Minor GC scans only dirty pages and shows improved pause times

### Implementation for User Story 1

- [ ] T008 [US1] Update `mark_minor_roots` to use dirty page snapshots in `crates/rudo-gc/src/gc/gc.rs`
- [ ] T009 [US1] Update `mark_minor_roots_multi` to use dirty page snapshots in `crates/rudo-gc/src/gc/gc.rs`
- [ ] T010 [US1] Update `mark_minor_roots_parallel` to use dirty page snapshots in `crates/rudo-gc/src/gc/gc.rs`

**Checkpoint**: User Story 1 is functional and minor GC scans only dirty pages

---

## Phase 4: User Story 3 - Correct Old-to-Young Reference Survival (Priority: P1)

**Goal**: Ensure old-to-young references remain correct under minor GC

**Independent Test**: Old-to-young reference survives minor GC, including large objects

### Tests for User Story 3

- [ ] T011 [P] [US3] Add dirty page list unit tests in `crates/rudo-gc/tests/dirty_page_list.rs`
- [ ] T012 [P] [US3] Add minor GC integration tests (old→young, large object) in `crates/rudo-gc/tests/minor_gc_optimized.rs`

**Checkpoint**: Old-to-young survival verified with integration tests

---

## Phase 5: User Story 2 - Minimal Write Barrier Overhead (Priority: P2)

**Goal**: Validate write barrier overhead stays under 5%

**Independent Test**: Microbenchmark shows <5% overhead increase

### Tests for User Story 2

- [ ] T013 [P] [US2] Add write barrier microbenchmark in `crates/rudo-gc/benches/write_barrier_overhead.rs`

**Checkpoint**: Overhead validated against baseline

---

## Phase 6: User Story 4 - Thread-Safe Concurrent Access (Priority: P2)

**Goal**: Ensure dirty page tracking is safe under concurrency

**Independent Test**: Loom-based tests show no races; concurrent mutation test passes

### Tests for User Story 4

- [ ] T014 [P] [US4] Add loom concurrency tests in `crates/rudo-gc/tests/loom_dirty_page_list.rs`

**Checkpoint**: Concurrency verified under loom

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Validation, performance checks, and CI readiness

- [ ] T015 Run full test suite via `./test.sh`
- [ ] T016 Run clippy via `./clippy.sh`
- [ ] T017 Run Miri via `./miri-test.sh`
- [ ] T018 [P] Add minor GC pause benchmark in `crates/rudo-gc/benches/minor_gc_pause.rs`

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3+)**: Depend on Foundational completion
- **Polish (Phase 7)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Depends on Foundational tasks only
- **User Story 3 (P1)**: Depends on User Story 1 for behavior validation
- **User Story 2 (P2)**: Depends on Foundational tasks (write barrier changes) for benchmarking
- **User Story 4 (P2)**: Depends on Foundational tasks (dirty list infra)

### Parallel Opportunities

- T011 and T012 can run in parallel (different test files)
- T013 and T014 can run in parallel (bench vs loom tests)
- T018 can run in parallel with other polish tasks

---

## Parallel Example: User Story 3

```bash
Task: "Add dirty page list unit tests in crates/rudo-gc/tests/dirty_page_list.rs"
Task: "Add minor GC integration tests (old→young, large object) in crates/rudo-gc/tests/minor_gc_optimized.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: Confirm minor GC scans only dirty pages

### Incremental Delivery

1. Setup + Foundational → Foundation ready
2. Add User Story 1 → Validate performance improvement
3. Add User Story 3 → Validate correctness
4. Add User Story 2 → Validate overhead
5. Add User Story 4 → Validate concurrency
6. Polish/CI checks → ready to merge
