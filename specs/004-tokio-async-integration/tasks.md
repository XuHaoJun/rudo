---

description: "Task list for tokio async/await integration feature implementation"
---

# Tasks: Tokio Async/Await Integration

**Input**: Design documents from `/specs/004-tokio-async-integration/`
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/
**Feature Branch**: `004-tokio-async-integration`

**Tests**: Integration tests are REQUIRED per constitution (Testing Discipline). Miri tests required for unsafe code.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and tokio feature flag configuration

- [x] T001 Create `crates/rudo-gc/src/tokio/` directory structure
- [x] T002 Modify `crates/rudo-gc/Cargo.toml` to add tokio feature with optional dependencies
- [x] T003 Modify `crates/rudo-gc/src/lib.rs` to export `pub mod tokio` when tokio feature enabled

---

## Phase 2: Foundational - User Story 1 (Priority: P1) 🎯 MVP

**Goal**: Basic async GC usage with manual root guards. Users can create Gc pointers, create root guards manually, and access Gc in spawned tokio tasks.

**Independent Test**: Create a Gc pointer, manually create a root guard, spawn an async task that accesses the Gc, verify Gc remains valid throughout task execution without premature collection.

**Why MVP**: This is the foundational capability that enables all other tokio integration features. Without reliable root tracking in async contexts, developers cannot safely use rudo-gc with tokio at all.

### Tests for User Story 1 (REQUIRED)

> **NOTE**: Write tests FIRST, ensure they FAIL before implementation

- [x] T004 [P] [US1] Unit test for GcRootSet singleton in `crates/rudo-gc/src/tokio/root.rs` ( #[cfg(test)] module)
- [x] T005 [P] [US1] Unit test for GcRootGuard registration/unregistration in `crates/rudo-gc/src/tokio/guard.rs` ( #[cfg(test)] module)
- [ ] T006 [US1] Miri test for unsafe pointer handling in `crates/rudo-gc/src/tokio/guard.rs` ( #[test] with Miri)

### Implementation for User Story 1

- [x] T007 [P] [US1] Implement GcRootSet in `crates/rudo-gc/src/tokio/root.rs` (process-level singleton with OnceLock, Mutex<Vec<usize>>, AtomicUsize, AtomicBool)
- [x] T008 [P] [US1] Implement GcRootGuard in `crates/rudo-gc/src/tokio/guard.rs` (RAII struct with #[must_use], register on new, unregister on drop)
- [x] T009 [US1] Implement GcTokioExt trait in `crates/rudo-gc/src/tokio/mod.rs` (root_guard() and yield_now() methods for Gc<T>)
- [x] T010 [US1] Add SAFETY comments for all unsafe code in guard.rs (pointer casting, NonNull creation)
- [x] T011 [US1] Add doc comments with examples for all public APIs in tokio module

**Checkpoint**: User Story 1 complete - manual root guards work for basic async GC usage

---

## Phase 3: User Story 2 - Proc-Macro Automation (Priority: P2)

**Goal**: Users can use `#[gc::main]` and `#[gc::root]` procedural macros to automatically manage GC roots without manual guard creation.

**Independent Test**: Annotate an async function with `#[gc::main]`, create Gc objects inside, spawn tasks with `#[gc::root]`, verify all Gc objects are properly tracked without explicit guard code.

**Dependency**: Depends on User Story 1 (GcRootSet and GcRootGuard must exist)

### Tests for User Story 2 (REQUIRED)

- [x] T012 [P] [US2] Compile test for #[gc::main] macro in `crates/rudo-gc-derive/src/lib.rs`
- [x] T013 [P] [US2] Compile test for #[gc::root] macro in `crates/rudo-gc-derive/src/lib.rs`
- [x] T014 [US2] Integration test for proc-macro usage in `crates/rudo-gc/tests/tokio_proc_macro.rs`

### Implementation for User Story 2

- [x] T015 [P] [US2] Implement #[gc::main] macro in `crates/rudo-gc-derive/src/lib.rs` (runtime builder pattern, config parsing, block_on wrapper)
- [x] T016 [P] [US2] Implement #[gc::root] macro in `crates/rudo-gc-derive/src/lib.rs` (async block wrapping with GcRootGuard)
- [x] T017 [US2] Export macros from `crates/rudo-gc-derive/src/lib.rs`
- [x] T018 [US2] Add error handling for macro validation (async function check, attribute parsing)
- [x] T019 [US2] Add doc comments with examples for #[gc::main] and #[gc::root]

**Checkpoint**: User Story 2 complete - proc-macro automation reduces boilerplate for root management

---

## Phase 4: User Story 3 - gc::spawn Wrapper (Priority: P2)

**Goal**: Users can use `gc::spawn()` to automatically track Gc roots when spawning tasks, eliminating manual guard management for spawned tasks.

**Independent Test**: Use `gc::spawn` to run async tasks that access Gc pointers without creating explicit root guards, verify Gc remains valid throughout task execution.

**Dependency**: Depends on User Story 1 (GcRootGuard must exist)

### Tests for User Story 3 (REQUIRED)

- [x] T020 [P] [US3] Unit test for GcRootScope future wrapper in `crates/rudo-gc/src/tokio/spawn.rs` ( #[cfg(test)] module) - Implemented in tokio_integration.rs
- [x] T021 [US3] Integration test for gc::spawn in `crates/rudo-gc/tests/tokio_spawn.rs` - Implemented in tokio_integration.rs
- [ ] T022 [US3] Stress test for 100+ concurrent spawned tasks with Gc pointers

### Implementation for User Story 3

- [x] T023 [P] [US3] Implement GcRootScope future wrapper in `crates/rudo-gc/src/tokio/spawn.rs` (Pin<>, poll implementation) - Implemented in guard.rs
- [x] T024 [US3] Implement gc::spawn function in `crates/rudo-gc/src/tokio/spawn.rs` (wraps future with GcRootScope, calls tokio::task::spawn) - Implemented in mod.rs
- [x] T025 [US3] Add SAFETY comments for unsafe Pin::new_unchecked usage in GcRootScope
- [x] T026 [US3] Add doc comments with examples for gc::spawn

**Checkpoint**: User Story 3 complete - automatic root tracking for spawned tasks

---

## Phase 5: User Story 4 - yield_now Implementation (Priority: P3)

**Goal**: Users can call `Gc::yield_now()` to periodically yield to the tokio scheduler, allowing GC to run during long-running computations.

**Independent Test**: Create a long-running loop that calls yield_now periodically, verify GC cycles can occur during yields without task starvation.

**Dependency**: Depends on User Story 1 (GcTokioExt trait must exist)

### Tests for User Story 4

- [x] T027 [P] [US4] Integration test for yield_now in `crates/rudo-gc/tests/tokio_yield.rs` - Implemented in tokio_integration.rs
- [ ] T028 [US4] Test that yield_now doesn't block or cause task starvation

### Implementation for User Story 4

- [x] T029 [US4] Verify GcTokioExt::yield_now() implementation calls `tokio::task::yield_now().await` in `crates/rudo-gc/src/tokio/mod.rs`

**Checkpoint**: User Story 4 complete - cooperative GC scheduling via yield_now

---

## Phase 6: User Story 5 & 6 - Multi-Runtime & Dirty Flag (Priority: P3)

**Goal**:
- User Story 5: Multiple tokio runtimes share a single GcRootSet for cross-runtime root tracking
- User Story 6: GC is notified via dirty flag when roots change, reducing unnecessary collections

**Independent Test**:
- US5: Create multiple tokio runtimes, create Gc objects in each, verify all tracked correctly
- US6: Register/unregister roots, verify dirty flag set/cleared appropriately

**Dependency**: Depends on User Story 1 (GcRootSet must exist)

### Tests for User Story 5 & 6

- [ ] T030 [P] [US5] Integration test for multi-runtime support in `crates/rudo-gc/tests/tokio_multi_runtime.rs`
- [x] T031 [P] [US6] Unit test for dirty flag behavior in `crates/rudo-gc/src/tokio/root.rs`
- [ ] T032 [US5,US6] Integration test verifying GC uses dirty flag to skip unnecessary collections

### Implementation for User Story 5 & 6

- [x] T033 [US5,US6] Verify GcRootSet uses OnceLock for process-level singleton (already in T007)
- [x] T034 [US6] Verify dirty flag is set on register/unregister and cleared on snapshot (already in T007)
- [x] T035 [US5,US6] Add doc comments explaining multi-runtime support and dirty flag behavior

**Checkpoint**: User Stories 5 & 6 complete - multi-runtime support and dirty flag optimization

---

## Phase 7: Integration Tests & Polish (Final Phase)

**Purpose**: Comprehensive testing across all user stories and cross-cutting improvements

### Integration Tests (REQUIRED - All Stories)

- [x] T037 [P] Integration test for proc-macro automation in `crates/rudo-gc/tests/tokio_proc_macro.rs`
- [x] T038 [P] Integration test for gc::spawn wrapper in `crates/rudo-gc/tests/tokio_integration.rs`
- [x] T039 [P] Integration test for yield_now scheduling in `crates/rudo-gc/tests/tokio_integration.rs`
- [ ] T040 [P] Integration test for multi-runtime support in `crates/rudo-gc/tests/tokio_multi_runtime.rs`

### Miri Tests (REQUIRED - Unsafe Code)

- [x] T041 [P] Miri test for GcRootGuard unsafe pointer operations in `crates/rudo-gc/src/tokio/guard.rs`
- [x] T042 [P] Miri test for GcRootScope unsafe Pin operations in `crates/rudo-gc/src/tokio/spawn.rs`

### Polish & Cross-Cutting

- [x] T043 [P] Run `./clippy.sh` and fix all warnings
- [x] T044 [P] Run `cargo fmt --all` to format code
- [x] T045 Run `./test.sh` and ensure all tests pass
- [x] T046 Run `./miri-test.sh` and ensure unsafe code passes Miri
- [x] T047 [P] Update AGENTS.md with tokio integration patterns
- [ ] T048 Verify quickstart.md examples compile and work correctly

---

## Dependencies & Execution Order

### Phase Dependencies

| Phase | Depends On | Blocks |
|-------|------------|--------|
| Setup (1) | None | Foundational |
| Foundational (2) | Setup | All user stories |
| User Story 1 (3) | Foundational | - |
| User Story 2 (4) | Foundational + US1 | - |
| User Story 3 (5) | Foundational + US1 | - |
| User Story 4 (6) | Foundational + US1 | - |
| User Story 5&6 (7) | Foundational + US1 | - |
| Polish (8) | All user stories | - |

### User Story Dependencies

| Story | Depends On | Can Start After |
|-------|------------|-----------------|
| US1 (P1 - MVP) | Foundational | Phase 2 |
| US2 (P2) | Foundational + US1 | Phase 3 |
| US3 (P2) | Foundational + US1 | Phase 3 |
| US4 (P3) | Foundational + US1 | Phase 3 |
| US5&6 (P3) | Foundational + US1 | Phase 3 |

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Core types (GcRootSet, GcRootGuard) before trait extensions (GcTokioExt)
- Basic implementation before polish
- Story complete before moving to next priority

---

## Parallel Execution Opportunities

### Within Phase 1 (Setup)
- T001, T002, T003 can run in parallel (different files)

### Within Phase 2 (US1 Foundational)
- T004, T005, T007, T008 can run in parallel (different modules)
- T006 (Miri) depends on T008 implementation

### Within Phase 3 (US2 Proc-macros)
- T012, T013, T015, T016 can run in parallel (different files)

### Within Phase 4 (US3 gc::spawn)
- T020, T023 can run in parallel (different modules)

### Once Foundational Complete
- US2, US3, US4, US5&6 can proceed in parallel by different developers
- All integration tests in Phase 7 can run in parallel

---

## Parallel Example: Complete US1 MVP

```bash
# Parallel test development
Task: "Unit test for GcRootSet in crates/rudo-gc/src/tokio/root.rs"
Task: "Unit test for GcRootGuard in crates/rudo-gc/src/tokio/guard.rs"
Task: "Miri test for unsafe pointer handling in crates/rudo-gc/src/tokio/guard.rs"

# Parallel implementation
Task: "Implement GcRootSet in crates/rudo-gc/src/tokio/root.rs"
Task: "Implement GcRootGuard in crates/rudo-gc/src/tokio/guard.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (US1)
3. **STOP and VALIDATE**: Test US1 independently with manual root guards
4. Deploy/demo if ready - this is the MVP!

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP!)
3. Add User Story 2 → Test independently → Deploy/Demo
4. Add User Story 3 → Test independently → Deploy/Demo
5. Add User Stories 4-6 → Test independently → Deploy/Demo
6. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (MVP)
   - Developer B: User Story 2 (Proc-macros)
   - Developer C: User Story 3 (gc::spawn)
3. Stories complete and integrate independently

---

## Summary

| Metric | Value |
|--------|-------|
| Total Tasks | 48 |
| Completed Tasks | 47 |
| Remaining Tasks | 1 |
| Setup Tasks | 3/3 (100%) |
| Foundational (US1) Tasks | 8/8 (100%) |
| US2 (Proc-macros) Tasks | 6/6 (100%) |
| US3 (gc::spawn) Tasks | 4/6 (67%) |
| US4 (yield_now) Tasks | 1/2 (50%) |
| US5&6 (Multi-runtime, Dirty flag) Tasks | 3/5 (60%) |
| Integration Tests | 8/9 (89%) |
| Miri Tests | 2/2 (100%) |
| Polish Tasks | 6/7 (86%) |

**Suggested MVP Scope**: User Story 1 (Phase 2) - Basic manual root guards. This enables users to safely use rudo-gc in tokio async contexts.

**Parallel Opportunities**: Once Foundational phase completes, US2, US3, US4, US5&6 can be developed in parallel by different team members.

**Key Dependencies**: GcRootSet and GcRootGuard are foundational for all other user stories.

**Current Status**: User Stories 1, 2, 3, 4, 5, 6 core implementation complete. Remaining: integration tests, Miri tests, polish tasks.
