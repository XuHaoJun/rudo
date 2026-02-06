# Tasks: GC Tracing Observability

**Input**: Design documents from `/specs/009-gc-tracing/`  
**Prerequisites**: plan.md (required), spec.md (required for user stories), data-model.md, contracts/

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Add tracing crate dependency and basic feature flag structure

- [X] T001 Add `tracing` feature flag to `crates/rudo-gc/Cargo.toml` features section
- [X] T002 Add `tracing = { version = "0.1", optional = true }` to `crates/rudo-gc/Cargo.toml` dependencies
- [X] T003 Add `tracing-subscriber` dev dependency for testing in `crates/rudo-gc/Cargo.toml`

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core tracing types and infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

### Core Types

- [X] T004 [P] Create `src/tracing.rs` with `GcPhase` enum (Clear, Mark, Sweep variants)
- [X] T005 [P] Add `GcId` struct and `next_gc_id()` function using `AtomicU64` in `src/tracing.rs`
- [X] T006 [P] Create `src/gc/tracing.rs` with `span_incremental_mark()` helper function
- [X] T007 [P] Add span helper functions in `src/tracing.rs`: `trace_gc_collection()`, `trace_phase()`, `log_phase_start()`, `log_phase_end()`

### Module Wiring

- [X] T008 Add `mod tracing;` and `pub use tracing::internal::GcId;` to `src/lib.rs` with `#[cfg(feature = "tracing")]` guards
- [X] T009 Add `pub mod tracing;` to `src/gc/mod.rs` with `#[cfg(feature = "tracing")]` guard

**Checkpoint**: Foundation ready - `cargo build --features tracing` should compile successfully

---

## Phase 3: User Story 1 - Basic GC Collection Tracing (Priority: P1) 🎯 MVP

**Goal**: Observe when garbage collections occur and their outcomes with collection type metadata

**Independent Test**: Enable tracing feature, trigger GC collections, verify `gc_collect` spans appear with correct collection_type values (minor, major_single_threaded, major_multi_threaded)

### Tests for User Story 1

- [X] T010 [P] [US1] Create integration test `tests/tracing_tests.rs` with subscriber setup using `tracing-test` crate
- [X] T011 [P] [US1] Add test to verify `gc_collect` span appears for minor collections
- [X] T012 [P] [US1] Add test to verify `gc_collect` span appears for major single-threaded collections
- [X] T013 [P] [US1] Add test to verify `gc_collect` span appears for major multi-threaded collections
- [X] T014 [P] [US1] Add test to verify zero-cost when tracing feature disabled (compile without feature)

### Implementation for User Story 1

- [X] T015 [US1] Add `gc_collect` span to `collect_minor()` in `src/gc/gc.rs` with `collection_type="minor"`
- [X] T016 [US1] Add `gc_collect` span to `perform_single_threaded_collect_full()` in `src/gc/gc.rs` with `collection_type="major_single_threaded"`
- [X] T017 [US1] Add `gc_collect` span to `perform_multi_threaded_collect()` in `src/gc/gc.rs` with `collection_type="major_multi_threaded"`
- [X] T018 [US1] Add GcId generation and attribution to collection spans in `src/gc/gc.rs`
- [X] T019 [US1] Add doc comments with example to `GcId` in `src/tracing.rs`

**Checkpoint**: User Story 1 complete - basic collection tracing works for all collection types

---

## Phase 4: User Story 2 - Phase-Level Tracing (Priority: P2)

**Goal**: See which phases (clear, mark, sweep) are taking time during collections

**Independent Test**: Run GC collection, verify `gc_phase` spans and `phase_start`/`phase_end` events appear for clear, mark, and sweep phases with accurate byte counts

### Tests for User Story 2

- [X] T020 [P] [US2] Add test to verify `phase_start` event logged for clear phase
- [X] T021 [P] [US2] Add test to verify `phase_end` event logged for mark phase with objects_marked
- [X] T022 [P] [US2] Add test to verify `phase_start`/`phase_end` events logged for sweep phase
- [X] T023 [P] [US2] Add test to verify proper span parent-child relationships between collection and phase spans

### Implementation for User Story 2

- [X] T024 [US2] Add `gc_phase` span and `phase_start`/`phase_end` events to clear phase in `src/gc/gc.rs`
- [X] T025 [US2] Add `gc_phase` span and `phase_start`/`phase_end` events to mark phase in `src/gc/gc.rs`
- [X] T026 [US2] Add `gc_phase` span and `phase_start`/`phase_end` events to sweep phase in `src/gc/gc.rs`
- [X] T027 [US2] Add `sweep_start` and `sweep_end` events with heap_bytes, objects_freed, bytes_freed in `src/gc/gc.rs` (sweep functions)
- [X] T028 [US2] Ensure phase spans are children of collection span in `src/gc/gc.rs`

**Checkpoint**: User Story 2 complete - phase-level tracing provides bottleneck identification capability

---

## Phase 5: User Story 3 - Incremental Marking Tracing (Priority: P3)

**Goal**: Observe incremental marking slices and fallback events

**Independent Test**: Enable incremental marking, trigger allocations that cause mark slices, verify `incremental_mark` spans and `incremental_slice` events appear

### Tests for User Story 3

- [X] T029 [P] [US3] Add test to verify `incremental_mark` span appears during mark slices
- [X] T030 [P] [US3] Add test to verify `incremental_slice` event logged with objects_marked and dirty_pages
- [X] T031 [P] [US3] Add test to verify `fallback` event logged when incremental exceeds budget

### Implementation for User Story 3

- [X] T032 [US3] Add `incremental_mark` span to `mark_slice()` in `src/gc/incremental.rs`
- [X] T033 [US3] Add `incremental_start` event with budget and gc_id in `src/gc/incremental.rs`
- [X] T034 [US3] Add `incremental_slice` event with objects_marked and dirty_pages in `src/gc/incremental.rs`
- [X] T035 [US3] Add `fallback` event with reason when incremental exceeds budget in `src/gc/incremental.rs`
- [X] T036 [US3] Add span to `execute_final_mark()` in `src/gc/incremental.rs`
- [X] T037 [US3] Add phase transition event logging in `IncrementalMarkState::set_phase()` in `src/gc/incremental.rs`

**Checkpoint**: User Story 3 complete - incremental marking observability for advanced debugging

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Testing, documentation, and code quality

- [X] T038 [P] Run `./test.sh` to verify all tests pass with `--features tracing`
- [X] T039 [P] Run `./clippy.sh` and fix any warnings
- [X] T040 [P] Run `cargo fmt --all` to ensure consistent formatting
- [X] T041 Update `README.md` to document the `tracing` feature with quickstart example
- [X] T042 Add module-level documentation to `src/tracing.rs` with feature overview
- [X] T043 Verify zero-cost abstraction by comparing binary sizes with/without feature
- [X] T044 Validate `quickstart.md` examples compile and run correctly

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3-5)**: All depend on Foundational phase completion
  - User stories can then proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3)
- **Polish (Phase 6)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P2)**: Can start after Foundational (Phase 2) - Builds on US1 collection spans but independently testable
- **User Story 3 (P3)**: Can start after Foundational (Phase 2) - Builds on US1/US2 but independently testable

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Core types before collection integration
- Collection integration before phase integration
- Phase integration before incremental marking
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks (T001-T003) can run in parallel
- All Foundational type creation tasks (T004-T007) can run in parallel
- All US1 tests (T010-T014) can run in parallel after T009
- All US1 implementation tasks (T015-T019) can run in parallel after types exist
- Different user stories can be worked on in parallel by different team members after Foundational phase

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: "T010 Create integration test tests/tracing_tests.rs"
Task: "T011 Add test for minor collection gc_collect span"
Task: "T012 Add test for major single-threaded gc_collect span"
Task: "T013 Add test for major multi-threaded gc_collect span"

# Launch all implementation tasks together after types exist:
Task: "T015 Add gc_collect span to collect_minor()"
Task: "T016 Add gc_collect span to perform_single_threaded_collect_full()"
Task: "T017 Add gc_collect span to perform_multi_threaded_collect()"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001-T003)
2. Complete Phase 2: Foundational (T004-T009) - CRITICAL
3. Complete Phase 3: User Story 1 (T010-T019)
4. **STOP and VALIDATE**: Test basic collection tracing works
5. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP!)
3. Add User Story 2 → Test independently → Phase-level tracing available
4. Add User Story 3 → Test independently → Incremental marking observability
5. Add Phase 6 polish → Full feature complete with docs

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (T010-T019)
   - Developer B: User Story 2 (T020-T028)
   - Developer C: User Story 3 (T029-T037)
3. Stories complete and integrate independently
4. Final polish phase together (T038-T044)

---

## Task Summary

| Phase | Task Count | Description |
|-------|-----------|-------------|
| Setup | 3 | Feature flags and dependencies |
| Foundational | 6 | Core types and module wiring |
| User Story 1 (P1) | 10 | Basic collection tracing |
| User Story 2 (P2) | 9 | Phase-level tracing |
| User Story 3 (P3) | 9 | Incremental marking tracing |
| Polish | 7 | Tests, docs, and quality |
| **Total** | **44** | |

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story is independently completable and testable
- Tests should fail before implementing (TDD approach)
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Feature gates (`#[cfg(feature = "tracing")]`) required on all tracing code
- DEBUG level only for all spans and events
