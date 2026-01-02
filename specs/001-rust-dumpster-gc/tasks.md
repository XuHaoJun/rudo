# Tasks: Rust Dumpster GC - BiBOP & Mark-Sweep Engine

**Input**: Design documents from `/specs/001-rust-dumpster-gc/`
**Prerequisites**: plan.md ✓, spec.md ✓, research.md ✓, data-model.md ✓, contracts/ ✓, quickstart.md ✓

**Tests**: Tests are MANDATORY per the project constitution. TDD approach with Miri validation.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Multi-crate workspace**:
  - `crates/rudo-gc/src/` - Main GC library
  - `crates/rudo-gc-derive/src/` - Proc-macro crate
  - `crates/rudo-gc/tests/` - Integration tests

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and workspace structure

- [x] T001 Create workspace `Cargo.toml` at repository root with `crates/rudo-gc` and `crates/rudo-gc-derive` members
- [x] T002 [P] Create `crates/rudo-gc/Cargo.toml` with dependencies (`proc-macro2`, re-export of derive macro)
- [x] T003 [P] Create `crates/rudo-gc-derive/Cargo.toml` with proc-macro dependencies (`syn`, `quote`, `proc-macro2`)
- [x] T004 [P] Create empty `crates/rudo-gc/src/lib.rs` with module declarations and public exports
- [x] T005 [P] Create empty `crates/rudo-gc-derive/src/lib.rs` with proc-macro scaffold
- [x] T006 Configure `rustfmt.toml` and `clippy.toml` at workspace root

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

### Memory Layout (BiBOP)

- [x] T007 Define page constants (`PAGE_SIZE`, `PAGE_MASK`, `MAGIC_GC_PAGE`) in `crates/rudo-gc/src/heap.rs`
- [x] T008 Implement `PageHeader` struct with magic, block_size, obj_count, generation, flags, mark_bitmap, free_list_head fields in `crates/rudo-gc/src/heap.rs`
- [x] T009 Implement `Segment<const BLOCK_SIZE: usize>` struct with pages, current_page, bump_ptr, bump_end fields in `crates/rudo-gc/src/heap.rs`
- [x] T010 Implement `Segment::new()` - allocate aligned page, initialize free list in `crates/rudo-gc/src/heap.rs`
- [x] T011 Implement `Segment::allocate()` - O(1) bump allocation with free-list fallback in `crates/rudo-gc/src/heap.rs`

### GlobalHeap & Size Class Routing

- [x] T012 Define size class constants (16, 32, 64, 128, 256, 512, 1024, 2048) in `crates/rudo-gc/src/heap.rs`
- [x] T013 Implement `GlobalHeap` struct with segments array and large_objects vec in `crates/rudo-gc/src/heap.rs`
- [x] T014 Implement compile-time size class routing using const generics trait `SizeClass` in `crates/rudo-gc/src/heap.rs`
- [x] T015 Implement `GlobalHeap::alloc<T>()` - route to correct segment based on `T::CLASS` in `crates/rudo-gc/src/heap.rs`
- [x] T016 Implement thread-local `HEAP: RefCell<GlobalHeap>` in `crates/rudo-gc/src/heap.rs`

### Core Types

- [x] T017 [P] Define `GcBox<T>` struct with ref_count and value fields in `crates/rudo-gc/src/ptr.rs`
- [x] T018 [P] Define `Nullable<T>` type alias for optional NonNull in `crates/rudo-gc/src/ptr.rs`
- [x] T019 Implement `Gc<T>` struct with `ptr: Cell<Nullable<GcBox<T>>>` in `crates/rudo-gc/src/ptr.rs`

### Trace Infrastructure

- [x] T020 Define `unsafe trait Trace` with `fn trace(&self, visitor: &mut impl Visitor)` in `crates/rudo-gc/src/trace.rs`
- [x] T021 Define `trait Visitor` with `fn visit<T: Trace + ?Sized>(&mut self, gc: &Gc<T>)` in `crates/rudo-gc/src/trace.rs`
- [x] T022 Implement blanket `Trace` for primitives (i8-i128, u8-u128, f32, f64, bool, char, ()) in `crates/rudo-gc/src/trace.rs`
- [x] T023 Implement `Trace` for std types (String, Box, Vec, Option, Result, RefCell, Cell) in `crates/rudo-gc/src/trace.rs`

**Checkpoint**: Foundation ready - user story implementation can now begin

---

## Phase 3: User Story 1 - Basic Allocation and Collection (Priority: P1) 🎯 MVP

**Goal**: Allocate objects in a garbage-collected heap using `Gc<T>` API, automatically reclaim when unreachable.

**Independent Test**: Allocate objects, drop all references (roots), trigger collection, verify memory is reclaimed or objects are finalized.

### Tests for User Story 1 (MANDATORY) ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [x] T024 [P] [US1] Create basic allocation test (alloc `Gc<i32>`, verify deref) in `crates/rudo-gc/tests/basic.rs`
- [x] T025 [P] [US1] Create drop/collection test (drop Gc, call collect(), verify freed) in `crates/rudo-gc/tests/basic.rs`
- [x] T026 [P] [US1] Create cycle collection test (A->B->A cycle, drop, collect, verify both freed) in `crates/rudo-gc/tests/cycles.rs`
- [x] T027 [P] [US1] Create Miri test configuration for UB detection in `crates/rudo-gc/tests/miri.rs`

### Implementation for User Story 1

#### Root Tracking (Shadow Stack)

- [x] T028 [US1] Implement `ShadowStack` struct with roots vec and frame_markers in `crates/rudo-gc/src/roots.rs`
- [x] T029 [US1] Implement `ShadowStack::push(ptr)` and `pop(ptr)` for root registration in `crates/rudo-gc/src/roots.rs`
- [x] T030 [US1] Implement `ShadowStack::iter()` for marking phase traversal in `crates/rudo-gc/src/roots.rs`
- [x] T031 [US1] Implement thread-local `SHADOW_STACK: RefCell<ShadowStack>` in `crates/rudo-gc/src/roots.rs`

#### Gc<T> Smart Pointer

- [x] T032 [US1] Implement `Gc::new(value)` - allocate in heap, register root in `crates/rudo-gc/src/ptr.rs`
- [x] T033 [US1] Implement `Deref for Gc<T>` with dead-check panic in `crates/rudo-gc/src/ptr.rs`
- [x] T034 [US1] Implement `Clone for Gc<T>` with ref_count increment in `crates/rudo-gc/src/ptr.rs`
- [x] T035 [US1] Implement `Drop for Gc<T>` with ref_count decrement and conditional collection trigger in `crates/rudo-gc/src/ptr.rs`
- [x] T036 [US1] Implement `Gc::try_deref()`, `Gc::try_clone()`, `Gc::is_dead()` safe accessors in `crates/rudo-gc/src/ptr.rs`
- [x] T037 [US1] Implement `Gc::ptr_eq()`, `Gc::ref_count()`, `Gc::as_ptr()` utility methods in `crates/rudo-gc/src/ptr.rs`

#### Mark-Sweep Collector

- [x] T038 [US1] Implement `MarkVisitor` struct implementing `Visitor` trait in `crates/rudo-gc/src/gc.rs`
- [x] T039 [US1] Implement mark phase - traverse from roots, set bits in page header mark_bitmap in `crates/rudo-gc/src/gc.rs`
- [x] T040 [US1] Implement sweep phase - iterate all pages, reclaim unmarked objects, rebuild free lists in `crates/rudo-gc/src/gc.rs`
- [x] T041 [US1] Implement `collect()` public function - orchestrate mark-sweep in `crates/rudo-gc/src/gc.rs`

#### Collection Condition

- [x] T042 [US1] Implement `CollectInfo` struct with n_gcs_dropped, n_gcs_existing, heap_size in `crates/rudo-gc/src/gc.rs`
- [x] T043 [US1] Implement `default_collect_condition(info)` - collect when dropped > existing in `crates/rudo-gc/src/gc.rs`
- [x] T044 [US1] Implement `set_collect_condition(fn)` to configure trigger logic in `crates/rudo-gc/src/gc.rs`

#### Public API Exports

- [x] T045 [US1] Export public types (`Gc`, `Trace`, `Visitor`, `CollectInfo`) from `crates/rudo-gc/src/lib.rs`
- [x] T046 [US1] Export public functions (`collect`, `set_collect_condition`, `default_collect_condition`) from `crates/rudo-gc/src/lib.rs`

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently. MVP achieved!

---

## Phase 4: User Story 2 - Custom Types with Trace (Priority: P1)

**Goal**: Store custom structs in the GC heap with automatically derived tracing logic.

**Independent Test**: Define a struct with `#[derive(Trace)]`, allocate it, ensure inner Gc pointers are followed during collection.

### Tests for User Story 2 (MANDATORY) ⚠️

- [x] T047 [P] [US2] Create derive test for struct with Gc field, verify tracing in `crates/rudo-gc/tests/derive.rs`
- [x] T048 [P] [US2] Create derive test for nested structs with multiple Gc fields in `crates/rudo-gc/tests/derive.rs`
- [x] T049 [P] [US2] Create derive test for enum variants with Gc fields in `crates/rudo-gc/tests/derive.rs`
- [x] T050 [P] [US2] Create derive test for generic types with Trace bounds in `crates/rudo-gc/tests/derive.rs`

### Implementation for User Story 2

#### Derive Macro

- [x] T051 [US2] Implement struct parsing with `syn` in `crates/rudo-gc-derive/src/lib.rs`
- [x] T052 [US2] Implement `trace()` code generation for named struct fields using `quote` in `crates/rudo-gc-derive/src/lib.rs`
- [x] T053 [US2] Implement `trace()` code generation for tuple struct fields in `crates/rudo-gc-derive/src/lib.rs`
- [x] T054 [US2] Implement enum variant parsing and trace generation in `crates/rudo-gc-derive/src/lib.rs`
- [x] T055 [US2] Implement generic type handling with automatic `T: Trace` bounds in `crates/rudo-gc-derive/src/lib.rs`
- [x] T056 [US2] Register `#[proc_macro_derive(Trace)]` and export from `crates/rudo-gc-derive/src/lib.rs`

#### Integration

- [x] T057 [US2] Re-export `Trace` derive macro from `crates/rudo-gc/src/lib.rs` using `pub use rudo_gc_derive::Trace`

**Checkpoint**: At this point, User Stories 1 AND 2 should both work independently

---

## Phase 5: User Story 3 - BiBOP Memory Layout (Priority: P2)

**Goal**: Objects of significantly different sizes are allocated in different memory segments (BiBOP) for O(1) allocation and minimal fragmentation.

**Independent Test**: Inspect internal heap state after allocating objects of different sizes (16 bytes vs 64 bytes) to ensure they reside in different segments/pages.

### Tests for User Story 3 (MANDATORY) ⚠️

- [x] T058 [P] [US3] Create size class routing test (verify u64 and [u64; 8] in different segments) in `crates/rudo-gc/tests/bibop.rs`
- [x] T059 [P] [US3] Create O(1) allocation validation test (measure allocation time consistency) in `crates/rudo-gc/tests/bibop.rs`
- [x] T060 [P] [US3] Create page header validation test (verify magic number, block_size, obj_count) in `crates/rudo-gc/tests/bibop.rs`
- [x] T061 [P] [US3] Create large object allocation test (objects > 2KB) in `crates/rudo-gc/tests/bibop.rs`

### Implementation for User Story 3

#### Large Object Space (LOS)

- [x] T062 [US3] Implement `GlobalHeap::alloc_large<T>()` for objects > 2048 bytes in `crates/rudo-gc/src/heap.rs`
- [x] T063 [US3] Implement LOS page tracking in `GlobalHeap.large_objects` in `crates/rudo-gc/src/heap.rs`
- [x] T064 [US3] Implement LOS sweep handling in mark-sweep collector in `crates/rudo-gc/src/gc.rs`

#### Zero-Sized Types

- [x] T065 [US3] Implement ZST detection and singleton handling for `Gc<()>` in `crates/rudo-gc/src/ptr.rs`
- [x] T066 [US3] Add ZST-specific Trace implementation in `crates/rudo-gc/src/trace.rs`

#### Interior Pointer Resolution

- [x] T067 [US3] Implement `ptr_to_page_header(ptr)` for O(1) page lookup via alignment in `crates/rudo-gc/src/heap.rs`
- [x] T068 [US3] Implement `ptr_to_object_index(ptr)` for interior pointer resolution in `crates/rudo-gc/src/heap.rs`
- [x] T069 [US3] Add interior pointer validation in conservative scanning path in `crates/rudo-gc/src/roots.rs`

#### Heap Introspection

- [x] T070 [US3] Implement `GlobalHeap::segment_for<T>()` debug accessor for verifying BiBOP routing in `crates/rudo-gc/src/heap.rs`

**Checkpoint**: All user stories should now be independently functional

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

### Self-Referential Structures

- [x] T071 [P] Implement `Gc::new_cyclic<F>()` for self-referential structures in `crates/rudo-gc/src/ptr.rs`
- [x] T072 [P] Add test for `new_cyclic` with self-referential struct in `crates/rudo-gc/tests/cycles.rs`

### Safety & Validation

- [x] T073 Implement out-of-memory handling via `handle_alloc_error()` in `crates/rudo-gc/src/heap.rs`
- [x] T074 Add alignment validation for all allocations in `crates/rudo-gc/src/heap.rs`
- [x] T075 Implement `!Send` and `!Sync` marker trait impls for `Gc<T>` in `crates/rudo-gc/src/ptr.rs`

### Documentation

- [x] T076 [P] Add comprehensive rustdoc to all public types in `crates/rudo-gc/src/lib.rs`
- [x] T077 [P] Add crate-level documentation with usage examples in `crates/rudo-gc/src/lib.rs`
- [x] T078 [P] Create README.md for `crates/rudo-gc/` with quickstart guide

### Performance Validation

- [ ] T079 [P] Create allocation benchmark (compare with Rc allocation time) in `crates/rudo-gc/benches/alloc.rs`
- [ ] T080 [P] Create collection benchmark (measure STW pause time) in `crates/rudo-gc/benches/collect.rs`
- [x] T081 Run Miri validation on all tests to verify no UB (passed with `-Zmiri-ignore-leaks`)

### Quickstart Validation

- [ ] T082 Validate quickstart.md examples compile and run correctly

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3-5)**: All depend on Foundational phase completion
  - US1 and US2 are both P1 and can proceed in parallel
  - US3 depends on US1 completion (needs working allocator)
- **Polish (Phase 6)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P1)**: Can start after Foundational (Phase 2) - Requires US1 `Gc<T>` for derive testing but no runtime dependency
- **User Story 3 (P2)**: Best started after US1 to leverage working heap - extends BiBOP internals

### Within Each User Story

- Tests (if included) MUST be written and FAIL before implementation
- Infrastructure before smart pointer
- Smart pointer before collection
- Core implementation before utilities
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel (T002-T005)
- All Foundational model tasks marked [P] can run in parallel (T017-T018)
- Once Foundational phase completes, US1 tests and US2 tests can start in parallel
- All test tasks for a user story marked [P] can run in parallel
- Different user stories can be worked on in parallel by different team members

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: T024 "Create basic allocation test in crates/rudo-gc/tests/basic.rs"
Task: T025 "Create drop/collection test in crates/rudo-gc/tests/basic.rs"
Task: T026 "Create cycle collection test in crates/rudo-gc/tests/cycles.rs"
Task: T027 "Create Miri test configuration in crates/rudo-gc/tests/miri.rs"

# After tests fail, launch root tracking and Gc implementation:
Task: T028-T031 (Shadow Stack) → then T032-T037 (Gc<T>)
Task: T038-T041 (Mark-Sweep) in parallel once T028-T031 complete
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: Test User Story 1 independently with `cargo test` and `cargo miri test`
5. Deploy/demo if ready - basic `Gc<T>` with cycle collection works!

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → **MVP Achieved!** (basic allocation + collection)
3. Add User Story 2 → Test independently → Now have ergonomic derive macro
4. Add User Story 3 → Test independently → Full BiBOP optimization validated
5. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers (per John McCarthy's 10-PhD team plan):

1. **Allocator Squad (3 devs)**: Phase 2 (T007-T016), then US3 (T062-T070)
2. **Tracing Squad (3 devs)**: T020-T023, then US1 roots (T028-T031), then US2 derive (T051-T057)
3. **Concurrency Squad (2 devs)**: US1 Mark-Sweep (T038-T044) - future parallel marking
4. **API Squad (2 devs)**: US1 Gc pointer (T032-T037), Phase 6 polish

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing (TDD)
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Run `cargo miri test` regularly to catch UB early
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
