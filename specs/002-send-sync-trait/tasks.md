---

description: "Task list template for feature implementation"
---

# Tasks: Send + Sync Trait Support

**Input**: Design documents from `/specs/002-send-sync-trait/`
**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **rudo-gc crate**: `crates/rudo-gc/src/`
- **Tests**: `tests/` at repository root
- **Documentation**: `specs/002-send-sync-trait/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure

- [ ] T001 Review existing ptr.rs structure in `crates/rudo-gc/src/ptr.rs` to understand current GcBox, Gc, Weak implementations
- [ ] T002 [P] Create test file structure at `tests/sync/mod.rs` for parallel test organization
- [ ] T003 [P] Review heap.rs mark_bitmap operations in `crates/rudo-gc/src/heap.rs` for memory ordering review

**Checkpoint**: Setup complete - foundational implementation can now begin

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**CRITICAL**: No user story work can begin until this phase is complete

- [ ] T010 Modify GcBox ref_count field from `Cell<NonZeroUsize>` to `AtomicUsize` in `crates/rudo-gc/src/ptr.rs:24`
- [ ] T011 [P] Modify GcBox weak_count field from `Cell<usize>` to `AtomicUsize` in `crates/rudo-gc/src/ptr.rs:26`
- [ ] T012 [P] Implement atomic inc_ref() method with Relaxed ordering in `GcBox` impl block at `crates/rudo-gc/src/ptr.rs`
- [ ] T013 Implement atomic dec_ref() method with AcqRel ordering and saturating overflow handling in `GcBox` at `crates/rudo-gc/src/ptr.rs`
- [ ] T014 [P] Implement atomic inc_weak() method with Relaxed ordering in `GcBox` at `crates/rudo-gc/src/ptr.rs`
- [ ] T015 Implement atomic dec_weak() method with AcqRel ordering in `GcBox` at `crates/rudo-gc/src/ptr.rs`

**Checkpoint**: Foundational ready - user story implementation can now begin

---

## Phase 3: User Story 1 - Multi-threaded Gc Pointer Sharing (Priority: P1) 🎯 MVP

**Goal**: Enable `Gc<T>` to be safely shared across threads with atomic reference counting

**Independent Test**: Clone `Gc<T>` on main thread, send to spawned thread, dereference successfully

### Implementation for User Story 1

- [ ] T020 [P] [US1] Modify Gc ptr field from `Cell<Nullable<GcBox<T>>>` to `AtomicPtr<GcBox<T>>` in `crates/rudo-gc/src/ptr.rs:268`
- [ ] T021 [US1] Update Gc::new() to use AtomicPtr store with Release ordering in `crates/rudo-gc/src/ptr.rs:292-323`
- [ ] T022 [US1] Update Gc::clone() to use AtomicPtr load with Acquire ordering in `crates/rudo-gc/src/ptr.rs:682-703`
- [ ] T023 [US1] Update Gc::deref() to use AtomicPtr load with Acquire ordering in `crates/rudo-gc/src/ptr.rs:672-680`
- [ ] T024 [US1] Add SAFETY comments to all unsafe blocks in Gc<T> methods per constitution at `crates/rudo-gc/src/ptr.rs`
- [ ] T025 [US1] Implement unsafe impl Send for Gc<T> where T: Trace + Send + Sync + ?Sized at end of `crates/rudo-gc/src/ptr.rs`
- [ ] T026 [US1] Implement unsafe impl Sync for Gc<T> where T: Trace + Send + Sync + ?Sized at end of `crates/rudo-gc/src/ptr.rs`

### Tests for User Story 1 (OPTIONAL - only if tests requested) ⚠️

> **NOTE: Write these tests to verify the implementation**

- [ ] T030 [P] [US1] Add compile-time Send/Sync assertion test at top of `tests/sync/mod.rs`
- [ ] T031 [US1] Add unit test for atomic inc_ref/dec_ref operations in `crates/rudo-gc/src/ptr.rs` #[cfg(test)] module
- [ ] T032 [P] [US1] Add integration test for cross-thread Gc clone/drop in `tests/sync/mod.rs`

**Checkpoint**: User Story 1 complete - Gc<T> can be shared across threads

---

## Phase 4: User Story 2 - Thread-safe Weak References (Priority: P2)

**Goal**: Enable `Weak<T>` to be safely used across threads for breaking cycles

**Independent Test**: Create Weak<T> on main thread, upgrade on spawned thread, verify correct behavior

### Implementation for User Story 2

- [ ] T040 [P] [US2] Modify Weak ptr field from `Cell<Nullable<GcBox<T>>>` to `AtomicPtr<GcBox<T>>` in `crates/rudo-gc/src/ptr.rs:845`
- [ ] T041 [US2] Update Weak::upgrade() to use AtomicPtr load with Acquire ordering in `crates/rudo-gc/src/ptr.rs:884-913`
- [ ] T042 [US2] Update Weak::clone() to use AtomicPtr operations in `crates/rudo-gc/src/ptr.rs:985-998`
- [ ] T043 [US2] Update Weak::is_alive() to use AtomicPtr load in `crates/rudo-gc/src/ptr.rs:935-942`
- [ ] T044 [US2] Add SAFETY comments to all unsafe blocks in Weak<T> methods at `crates/rudo-gc/src/ptr.rs`
- [ ] T045 [US2] Implement unsafe impl Send for Weak<T> where T: Trace + Send + Sync + ?Sized at end of `crates/rudo-gc/src/ptr.rs`
- [ ] T046 [US2] Implement unsafe impl Sync for Weak<T> where T: Trace + Send + Sync + ?Sized at end of `crates/rudo-gc/src/ptr.rs`

### Tests for User Story 2 (OPTIONAL - only if tests requested) ⚠️

- [ ] T050 [P] [US2] Add unit test for Weak upgrade/downgrade across threads in `tests/sync/mod.rs`
- [ ] T051 [US2] Add integration test for Weak::is_alive() thread safety in `tests/sync/mod.rs`

**Checkpoint**: User Story 2 complete - Weak<T> can be used across threads

---

## Phase 5: User Story 3 - Concurrent GC Operations (Priority: P3)

**Goal**: Ensure garbage collection works correctly when pointers are shared across threads

**Independent Test**: Run GC collection while multiple threads hold Gc pointers, verify no corruption

### Implementation for User Story 3

- [ ] T060 [P] [US3] Review and update PageHeader::set_mark memory ordering in `crates/rudo-gc/src/heap.rs` for future parallel marking compatibility
- [ ] T061 [US3] Verify Gc::drop() works correctly with atomic ref counting during concurrent access in `crates/rudo-gc/src/ptr.rs:705-734`
- [ ] T062 [US3] Add documentation comments for thread-safety guarantees at relevant method docs in `crates/rudo-gc/src/ptr.rs`

### Tests for User Story 3 (OPTIONAL - only if tests requested) ⚠️

- [ ] T070 [P] [US3] Add integration test for GC collection during concurrent access in `tests/sync/mod.rs`
- [ ] T071 [US3] Add stress test for concurrent clone/drop/collect operations in `tests/sync/mod.rs`

**Checkpoint**: User Story 3 complete - GC correctness under concurrent access verified

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [ ] T080 [P] Run `./clippy.sh` and fix all warnings in modified files
- [ ] T081 [P] Run `cargo fmt --all` to format all modified code
- [ ] T082 Run `./test.sh` and ensure all tests pass (including new tests)
- [ ] T083 Run `./miri-test.sh` and verify all Miri tests pass for atomic operations
- [ ] T084 [P] Update documentation in `crates/rudo-gc/src/lib.rs` if public API changes are exposed
- [ ] T085 Run ThreadSanitizer test to verify zero data races: `RUSTFLAGS="-Z sanitizer=thread" cargo test`

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3+)**: All depend on Foundational phase completion
  - User stories can proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3)
- **Polish (Final Phase)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P2)**: Can start after Foundational (Phase 2) - May integrate with US1 but should be independently testable
- **User Story 3 (P3)**: Can start after Foundational (Phase 2) - May integrate with US1/US2 but should be independently testable

### Within Each User Story

- Tests (if included) should be written and FAIL before implementation
- Core implementation before integration
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel
- All Foundational tasks marked [P] can run in parallel (within Phase 2)
- Once Foundational phase completes, all user stories can start in parallel (if team capacity allows)
- All tests for a user story marked [P] can run in parallel
- Polish tasks marked [P] can run in parallel

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: Test User Story 1 independently
5. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP!)
3. Add User Story 2 → Test independently → Deploy/Demo
4. Add User Story 3 → Test independently → Deploy/Demo
5. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1
   - Developer B: User Story 2
   - Developer C: User Story 3
3. Stories complete and integrate independently

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
