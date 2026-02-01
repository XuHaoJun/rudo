# Tasks: HandleScope v2 Implementation

**Input**: Design documents from `/home/noah/Desktop/rudo/specs/006-handlescope-v2-impl/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/

**Tests**: The examples below include test tasks. Tests are OPTIONAL - only include them if explicitly requested in the feature specification.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure for HandleScope v2 implementation

- [X] T001 Create handles module directory structure at `crates/rudo-gc/src/handles/`
- [X] T002 [P] Create handles submodule files: mod.rs, local_handles.rs, async.rs, tests/mod.rs
- [X] T003 [P] Initialize handles module exports in `crates/rudo-gc/src/handles/mod.rs`
- [X] T004 [P] Create integration test files at `crates/rudo-gc/tests/handlescope_*.rs`

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core data structures that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

### Foundational Implementation

- [X] T005 Create HandleSlot struct with gc_box_ptr field in `crates/rudo-gc/src/handles/local_handles.rs`
- [X] T006 [P] Create HandleBlock struct with 256 slots array in `crates/rudo-gc/src/handles/local_handles.rs`
- [X] T007 [P] Create HandleScopeData struct with next, limit, level, sealed_level fields in `crates/rudo-gc/src/handles/local_handles.rs`
- [X] T008 Create LocalHandles struct with blocks linked list and scope_data in `crates/rudo-gc/src/handles/local_handles.rs`
- [X] T009 Implement LocalHandles::new(), add_block(), allocate(), iterate() in `crates/rudo-gc/src/handles/local_handles.rs`
- [X] T010 Implement LocalHandles::scope_data_mut() in `crates/rudo-gc/src/handles/local_handles.rs`

### ThreadControlBlock Extensions

- [X] T011 Extend ThreadControlBlock with local_handles: UnsafeCell<LocalHandles> in `crates/rudo-gc/src/heap.rs`
- [X] T012 [P] Extend ThreadControlBlock with async_scopes: Mutex<Vec<AsyncScopeEntry>> in `crates/rudo-gc/src/heap.rs`
- [X] T013 Implement ThreadControlBlock::local_handles_mut() in `crates/rudo-gc/src/heap.rs`
- [X] T014 Implement ThreadControlBlock::register_async_scope() / unregister_async_scope() in `crates/rudo-gc/src/heap.rs`
- [X] T015 Implement ThreadControlBlock::iterate_all_handles() for GC root collection in `crates/rudo-gc/src/heap.rs`

### Foundational Tests

- [X] T016 [P] Unit tests for HandleSlot in `crates/rudo-gc/src/handles/tests/local_handles.rs`
- [X] T017 [P] Unit tests for HandleBlock in `crates/rudo-gc/src/handles/tests/local_handles.rs`
- [X] T018 [P] Unit tests for HandleScopeData in `crates/rudo-gc/src/handles/tests/local_handles.rs`
- [X] T019 [P] Unit tests for LocalHandles allocation in `crates/rudo-gc/src/handles/tests/local_handles.rs`
- [X] T020 [P] Unit tests for LocalHandles::iterate() in `crates/rudo-gc/src/handles/tests/local_handles.rs`

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - Create and Use Handles in Scope (Priority: P1) 🎯 MVP

**Goal**: Implement HandleScope<'env> and Handle<'scope, T> for compile-time lifetime-bound GC references

**Independent Test**: Can create a HandleScope, allocate Gc objects, create handles, verify handles work within scope, and verify handles become invalid after scope ends

### User Story 1 Implementation

- [X] T021 Create HandleScope struct in `crates/rudo-gc/src/handles/mod.rs`
- [X] T022 [P] Create Handle<'scope, T> struct with slot and PhantomData marker in `crates/rudo-gc/src/handles/mod.rs`
- [X] T023 Implement HandleScope::new(tcb) constructor in `crates/rudo-gc/src/handles/mod.rs`
- [X] T024 Implement HandleScope::handle() to create Handle from Gc<T> in `crates/rudo-gc/src/handles/mod.rs`
- [X] T025 Implement HandleScope drop to restore previous scope state in `crates/rudo-gc/src/handles/mod.rs`
- [X] T026 [P] Implement Handle::get() dereference in `crates/rudo-gc/src/handles/mod.rs`
- [X] T027 [P] Implement Handle::to_gc() conversion in `crates/rudo-gc/src/handles/mod.rs`
- [X] T028 [P] Implement Handle Deref, Copy, Clone traits in `crates/rudo-gc/src/handles/mod.rs`
- [X] T029 [P] Implement !Send + !Sync for Handle in `crates/rudo-gc/src/handles/mod.rs`

### User Story 1 Tests

- [X] T030 [P] [US1] Unit tests for HandleScope creation and lifecycle in `crates/rudo-gc/src/tests/handlescope_basic.rs`
- [X] T031 [P] [US1] Unit tests for Handle creation from Gc<T> in `crates/rudo-gc/src/tests/handlescope_basic.rs`
- [X] T032 [US1] Integration test for nested HandleScopes in `crates/rudo-gc/src/tests/handlescope_basic.rs`
- [X] T033 [US1] Integration test for handle invalidation on scope drop in `crates/rudo-gc/src/tests/handlescope_basic.rs`

**Checkpoint**: User Story 1 complete - basic HandleScope functionality is working

---

## Phase 4: User Story 2 - Escape Handles to Parent Scope (Priority: P2)

**Goal**: Implement EscapeableHandleScope<'env> for controlled handle escape to parent scope

**Independent Test**: Can create an EscapeableHandleScope, create a handle inside, call escape() to return it to parent scope, and verify escaped handle remains valid in outer scope

### User Story 2 Implementation

- [X] T034 Create EscapeableHandleScope struct with inner, escaped, escape_slot in `crates/rudo-gc/src/handles/mod.rs`
- [X] T035 [P] Create MaybeHandle<'scope, T> struct for optional handle pattern in `crates/rudo-gc/src/handles/mod.rs`
- [X] T036 Implement EscapeableHandleScope::new(tcb) constructor in `crates/rudo-gc/src/handles/mod.rs`
- [X] T037 Implement EscapeableHandleScope::handle() in `crates/rudo-gc/src/handles/mod.rs`
- [X] T038 Implement EscapeableHandleScope::escape() with single-use constraint in `crates/rudo-gc/src/handles/mod.rs`
- [X] T039 [P] Implement MaybeHandle::empty(), from_handle(), is_empty(), to_handle() in `crates/rudo-gc/src/handles/mod.rs`

### User Story 2 Tests

- [X] T040 [P] [US2] Unit tests for EscapeableHandleScope creation in `crates/rudo-gc/src/tests/handlescope_escape.rs`
- [X] T041 [US2] Integration test for escape pattern returning handle to outer scope in `crates/rudo-gc/src/tests/handlescope_escape.rs`
- [X] T042 [US2] Integration test for double-escape panic in `crates/rudo-gc/src/tests/handlescope_escape.rs`
- [X] T043 [P] [US2] Unit tests for MaybeHandle pattern in `crates/rudo-gc/src/tests/handlescope_escape.rs`

**Checkpoint**: User Story 2 complete - escape pattern is working

---

## Phase 5: User Story 3 - Prevent Handle Creation in Critical Sections (Priority: P3)

**Goal**: Implement SealedHandleScope<'env> for debug-only handle creation prevention

**Independent Test**: Can create SealedHandleScope in debug mode and verify handle creation attempts trigger panics; in release mode, it should be no-op

### User Story 3 Implementation

- [X] T044 Create SealedHandleScope struct (debug and release variants) in `crates/rudo-gc/src/handles/mod.rs`
- [X] T045 [P] Update HandleScopeData to include sealed_level field for debug assertions
- [X] T046 Implement SealedHandleScope::new(tcb) constructor in `crates/rudo-gc/src/handles/mod.rs`
- [X] T047 Update allocate_slot() to check sealed_level and panic in debug mode in `crates/rudo-gc/src/handles/local_handles.rs`

### User Story 3 Tests

- [X] T048 [P] [US3] Unit tests for SealedHandleScope in debug mode in `crates/rudo-gc/src/tests/handlescope_escape.rs`
- [X] T049 [US3] Integration test for handle creation panic in sealed scope in `crates/rudo-gc/src/tests/handlescope_escape.rs`
- [X] T050 [US3] Verification that SealedHandleScope is no-op in release mode in `crates/rudo-gc/src/tests/handlescope_escape.rs`

**Checkpoint**: User Story 3 complete - sealed scope functionality is working

---

## Phase 6: User Story 4 - Use GC Handles in Async/Await Code (Priority: P1)

**Goal**: Implement AsyncHandleScope and AsyncHandle<T> for async/await-safe handle management

**Independent Test**: Can create AsyncHandleScope, allocate Gc objects, create async handles, await operations, and verify handles remain valid across await points

### User Story 4 Implementation

- [X] T051 Create AsyncHandleScope struct with id, tcb, block, used, dropped in `crates/rudo-gc/src/handles/async.rs`
- [X] T052 [P] Create AsyncHandleGuard<'scope> struct for safe async handle access in `crates/rudo-gc/src/handles/async.rs`
- [X] T053 [P] Create AsyncHandle<T> struct with slot and scope_id in `crates/rudo-gc/src/handles/async.rs`
- [X] T054 Implement AsyncHandleScope::new(tcb) with ID generation and registration in `crates/rudo-gc/src/handles/async.rs`
- [X] T055 Implement AsyncHandleScope::handle() for creating AsyncHandle<T> in `crates/rudo-gc/src/handles/async.rs`
- [X] T056 Implement AsyncHandleScope::with_guard() for safe access pattern in `crates/rudo-gc/src/handles/async.rs`
- [X] T057 [P] Implement AsyncHandleScope::iterate() for GC root collection in `crates/rudo-gc/src/handles/async.rs`
- [X] T058 [P] Implement AsyncHandleGuard::get() for safe async handle access in `crates/rudo-gc/src/handles/async.rs`
- [X] T059 [P] Implement AsyncHandle::get() unsafe accessor in `crates/rudo-gc/src/handles/async.rs`
- [X] T060 [P] Implement AsyncHandle::to_gc() conversion in `crates/rudo-gc/src/handles/async.rs`
- [X] T061 [P] Implement Send + Sync for AsyncHandle in `crates/rudo-gc/src/handles/async.rs`

### User Story 4 Tests

- [X] T062 [P] [US4] Unit tests for AsyncHandleScope creation and lifecycle in `crates/rudo-gc/src/tests/handlescope_async.rs`
- [X] T063 [US4] Integration test for async handles across await points in `crates/rudo-gc/src/tests/handlescope_async.rs`
- [X] T064 [P] [US4] Unit tests for AsyncHandle creation in `crates/rudo-gc/src/tests/handlescope_async.rs`
- [X] T065 [US4] Integration test for AsyncHandleGuard safe access pattern in `crates/rudo-gc/src/tests/handlescope_async.rs`

**Checkpoint**: User Story 4 complete - async handle functionality is working

---

## Phase 7: User Story 5 - Safely Spawn Async Tasks with GC Roots (Priority: P1)

**Goal**: Implement spawn_with_gc! macro for ergonomic async task spawning with automatic root tracking

**Independent Test**: Can use spawn_with_gc! macro with single and multiple Gc objects, verifying handles remain valid throughout async task lifetime

### User Story 5 Implementation

- [X] T066 Create spawn_with_gc! macro for single Gc object in `crates/rudo-gc/src/handles/async.rs`
- [X] T067 [P] Extend spawn_with_gc! macro for multiple Gc objects in `crates/rudo-gc/src/handles/async.rs`
- [X] T068 Implement current_thread_control_block() helper in `crates/rudo-gc/src/heap.rs`
- [X] T069 [P] Export spawn_with_gc! macro from handles module in `crates/rudo-gc/src/handles/mod.rs`

### User Story 5 Tests

- [X] T070 [P] [US5] Unit test for spawn_with_gc! with single Gc in `crates/rudo-gc/src/tests/handlescope_async.rs`
- [X] T071 [US5] Integration test for spawn_with_gc! with multiple Gc objects in `crates/rudo-gc/src/tests/handlescope_async.rs`
- [X] T072 [US5] Integration test for GC during spawned task execution in `crates/rudo-gc/src/tests/handlescope_async.rs`

**Checkpoint**: User Story 5 complete - spawn_with_gc! macro is working

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [X] T073 [P] Update lib.rs exports to include all new HandleScope types and macros
- [X] T074 Update GC collect_roots() to use iterate_all_handles() in `crates/rudo-gc/src/gc.rs`
- [X] T075 [P] Add comprehensive doc comments with examples for all public APIs in `crates/rudo-gc/src/handles/mod.rs`
- [X] T076 [P] Add comprehensive doc comments with examples for async types in `crates/rudo-gc/src/handles/async.rs`
- [X] T077 Add SAFETY comments for all unsafe operations in `crates/rudo-gc/src/handles/`
- [X] T078 Run quickstart.md examples validation
- [X] T079 [P] Miri tests for unsafe code validation in `crates/rudo-gc/tests/handlescope_miri.rs`
- [X] T080 GC integration tests to verify handles are correctly tracked as roots in `crates/rudo-gc/tests/handlescope_integration.rs`
- [X] T081 [P] Run ./clippy.sh and fix all warnings
- [X] T082 [P] Run ./test.sh and ensure all tests pass

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3+)**: All depend on Foundational phase completion
  - User stories can then proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3 → P1 → P1)
- **Polish (Final Phase)**: Depends on all user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational - No dependencies on other stories
- **User Story 2 (P2)**: Can start after Foundational - No dependencies on other stories
- **User Story 3 (P3)**: Can start after Foundational - No dependencies on other stories
- **User Story 4 (P1)**: Can start after Foundational - No dependencies on other stories
- **User Story 5 (P1)**: Can start after Foundational - No dependencies on other stories (uses US4 types)

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Foundational types before HandleScope types
- Core implementation before integration tests
- Story complete before moving to polish phase

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel
- All Foundational tasks marked [P] can run in parallel (within Phase 2)
- Once Foundational phase completes, all user stories can start in parallel (if team capacity allows)
- All tests for a user story marked [P] can run in parallel
- Different user stories can be worked on in parallel by different team members

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: "Unit tests for HandleScope creation and lifecycle"
Task: "Unit tests for Handle creation from Gc<T>"

# Launch all models for User Story 1 together:
Task: "Create HandleScope struct"
Task: "Create Handle<'scope, T> struct"
```

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
5. Add User Story 4 → Test independently → Deploy/Demo
6. Add User Story 5 → Test independently → Deploy/Demo
7. Polish phase → Final deliverable
8. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (P1 - core)
   - Developer B: User Story 4 (P1 - async)
   - Developer C: User Story 5 (P1 - macro)
3. Stories complete and integrate independently

---

## Summary

- **Total Tasks**: 82
- **User Story 1 Tasks**: 14 (T021-T033)
- **User Story 2 Tasks**: 10 (T034-T043)
- **User Story 3 Tasks**: 6 (T044-T050)
- **User Story 4 Tasks**: 15 (T051-T065)
- **User Story 5 Tasks**: 7 (T066-T072)
- **Foundation Tasks**: 16 (T005-T020)
- **Setup Tasks**: 4 (T001-T004)
- **Polish Tasks**: 10 (T073-T082)

**MVP Scope**: Phases 1, 2, and 3 (User Story 1) = 33 tasks

**Parallel Opportunities**: 40+ tasks marked with [P] can run in parallel

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
- Run `./test.sh -- --test-threads=1` to avoid GC interference between parallel test threads
- Run `./miri-test.sh` for unsafe code validation
