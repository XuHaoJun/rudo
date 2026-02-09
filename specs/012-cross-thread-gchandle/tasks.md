# Implementation Tasks: Cross-Thread GC Handle System

**Feature**: Cross-Thread GC Handle System
**Branch**: `012-cross-thread-gchandle`
**Created**: 2026-02-10
**Plan**: [link to plan.md](plan.md)

## Overview

This document contains implementation tasks organized by user story to enable independent implementation and testing. Each user story represents a complete, independently testable increment.

## Dependency Graph

```
Phase 1: Setup
    ↓
Phase 2: Foundational
    ↓
Phase 3: [US1] Async UI Update Scheduling (P1) ← Core handle types + Gc::cross_thread_handle()
    ↓
Phase 4: [US2] Object Lifetime Management (P1) ← Root registration + GC integration
    ↓
Phase 5: [US3] Weak Cross-Thread References (P2) ← WeakCrossThreadHandle type
    ↓
Phase 6: [US4] Defensive Thread Handling (P2) ← try_resolve() + is_valid() polish
    ↓
Phase 7: Testing Strategy
    ↓
Phase 8: Polish & Documentation
```

**Story Dependencies**:
- US2 depends on US1 (requires GcHandle core types)
- US3 depends on US1 (requires HandleId and root table infrastructure)
- US4 depends on US1 (requires core handle infrastructure)
- US2, US3, US4 can proceed in parallel after Phase 2

## Parallel Execution Opportunities

| Stories | Can Execute In Parallel Because |
|---------|--------------------------------|
| US2, US3, US4 | All depend only on Phase 1-2; no interdependencies between stories |
| Module exports + GC integration | Different files, no dependencies |

---

## Phase 1: Setup

**Goal**: Initialize project structure and verify development environment

**Independent Test Criteria**: N/A (setup phase)

### Tasks

- [ ] T001 Create handles module directory at `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/`
- [ ] T002 Create `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/mod.rs` with module declarations
- [ ] T003 Create empty `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs` file for handle types
- [ ] T004 Create test file `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs` with test module structure
- [ ] T005 Run `./clippy.sh` to verify no warnings before starting
- [ ] T006 Run `cargo fmt --all` to ensure consistent formatting

---

## Phase 2: Foundational Infrastructure

**Goal**: Add core infrastructure that all user stories depend on

**Independent Test Criteria**: N/A (foundational phase - enables subsequent phases)

### Tasks

- [ ] T010 Add `CrossThreadRootTable` struct to `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs`:
  ```rust
  struct CrossThreadRootTable {
      next_id: u64,
      strong: HashMap<HandleId, NonNull<GcBox<()>>>,
  }
  ```
- [ ] T011 Add `HandleId` type to `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs` with INVALID sentinel:
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
  struct HandleId(u64);
  impl HandleId {
      const INVALID: HandleId = HandleId(u64::MAX);
  }
  ```
- [ ] T012 Add `cross_thread_roots: Mutex<CrossThreadRootTable>` field to `ThreadControlBlock` struct in `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs`
- [ ] T013 Implement `allocate_id()` method on `CrossThreadRootTable` in `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs`
- [ ] T014 Import `CrossThreadRootTable` and `HandleId` in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/mod.rs`
- [ ] T015 Verify compilation with `cargo build --workspace`

---

## Phase 3: [US1] Async UI Update Scheduling (P1)

**Goal**: Implement core GcHandle type with Send + Sync guarantees and origin-thread resolution

**Independent Test Criteria**: Handle can be created, sent between threads via channel, and resolved back to Gc<T> on origin thread

### Core Handle Types

- [ ] T020 [P] Define `GcHandle<T: Trace + 'static>` struct in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`:
  ```rust
  pub struct GcHandle<T: Trace + 'static> {
      ptr: NonNull<GcBox<T>>,
      origin_tcb: Arc<ThreadControlBlock>,
      origin_thread: ThreadId,
      handle_id: HandleId,
  }
  ```
- [ ] T021 [P] Implement `origin_thread()` method returning stored ThreadId
- [ ] T022 [P] Implement `is_valid()` method checking handle_id != HandleId::INVALID
- [ ] T023 [P] Implement `unregister(&mut self)` method:
  ```rust
  pub fn unregister(&mut self) {
      let mut roots = self.origin_tcb.cross_thread_roots.lock();
      roots.strong.remove(&self.handle_id);
      self.handle_id = HandleId::INVALID;
  }
  ```

### Send + Sync Implementation

- [ ] T024 Implement unsafe Send + Sync for GcHandle<T> in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs` with SAFETY comment:
  ```rust
  unsafe impl<T: Trace + 'static> Send for GcHandle<T> {}
  unsafe impl<T: Trace + 'static> Sync for GcHandle<T> {}
  ```

### Drop Implementation

- [ ] T025 Implement `Drop` for GcHandle in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`:
  ```rust
  impl<T: Trace + 'static> Drop for GcHandle<T> {
      fn drop(&mut self) {
          let mut roots = self.origin_tcb.cross_thread_roots.lock();
          roots.strong.remove(&self.handle_id);
      }
  }
  ```
  - Include SAFETY comment explaining thread-safe drop via Arc<TCB>

### Clone Implementation

- [ ] T026 Implement `Clone` for GcHandle in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`:
  ```rust
  impl<T: Trace + 'static> Clone for GcHandle<T> {
      fn clone(&self) -> Self {
          let mut roots = self.origin_tcb.cross_thread_roots.lock();
          let new_id = roots.allocate_id();
          roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
          GcHandle {
              ptr: self.ptr,
              origin_tcb: Arc::clone(&self.origin_tcb),
              origin_thread: self.origin_thread,
              handle_id: new_id,
          }
      }
  }
  ```

### Gc Extension Methods

- [ ] T027 Add `Gc::cross_thread_handle(&self)` method in `/home/noah/Desktop/rudo/crates/rudo-gc/src/ptr.rs` with atomic registration:
  ```rust
  pub fn cross_thread_handle<T: Trace + 'static>(&self) -> GcHandle<T> {
      let tcb = current_thread_tcb();
      let mut roots = tcb.cross_thread_roots.lock();
      let handle_id = roots.allocate_id();
      let ptr = self.as_non_null();
      roots.strong.insert(handle_id, ptr.cast::<GcBox<()>>());
      drop(roots);
      GcHandle {
          ptr,
          origin_tcb: Arc::clone(&tcb),
          origin_thread: std::thread::current().id(),
          handle_id,
      }
  }
  ```
  - Include SAFETY comment explaining atomic creation prevents GC during registration

### Debug Implementation

- [ ] T028 Implement `Debug` for GcHandle in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`

### Module Exports

- [ ] T029 Export `GcHandle` from `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/mod.rs`
- [ ] T030 Export `GcHandle` from `/home/noah/Desktop/rudo/crates/rudo-gc/src/lib.rs`

### US1 Tests

- [ ] T031 Write `test_cross_thread_send` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create Gc<T>, call cross_thread_handle(), send handle through mpsc channel, verify it arrives on other thread
- [ ] T032 Write `test_resolve_origin_thread` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create handle, spawn foreign thread, verify resolve() panics with thread ID mismatch message
- [ ] T033 Write `test_multiple_handles_same_object` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create multiple handles to same object, verify all resolve to same object
- [ ] T034 Run tests with `cargo test test_cross_thread_send test_resolve_origin_thread test_multiple_handles_same_object -- --test-threads=1`

---

## Phase 4: [US2] Object Lifetime Management (P1)

**Goal**: Implement GC integration so handles act as roots and keep objects alive

**Independent Test Criteria**: Strong handles prevent object collection; GC marks handle roots correctly

### GC Integration

- [ ] T040 Add `mark_cross_thread_roots()` function in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/gc.rs`:
  ```rust
  fn mark_cross_thread_roots(tcb: &ThreadControlBlock, visitor: &mut GcVisitor) {
      let roots = tcb.cross_thread_roots.lock();
      for (_id, ptr) in &roots.strong {
          unsafe { visitor.mark(*ptr); }
      }
  }
  ```
  - Include SAFETY comment explaining pointer validity guarantee
- [ ] T041 Call `mark_cross_thread_roots()` from `mark_all_roots()` in `/home/noah/Desktop/rudo/crates/rudo-gc/src/gc/gc.rs` during root scanning phase
- [ ] T042 Add lock ordering documentation comment to `/home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs`:
  ```rust
  // Lock ordering: LocalHeap → GlobalMarkState → GcRequest → CrossThreadRootTable
  ```

### US2 Tests

- [ ] T043 Write `test_handle_keeps_alive` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create Gc<T>, create handle, drop original Gc, force GC with `Gc::collect()`, verify handle resolves to live object
- [ ] T044 Write `test_clone_independent_lifetime` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Clone handle, drop original, verify cloned handle still keeps object alive
- [ ] T045 Write `test_drop_from_foreign_thread` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create handle, spawn foreign thread, drop handle in foreign thread, verify no panic
- [ ] T046 Write `test_origin_thread_exit` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create handle, verify handle is_valid() returns true, document expected behavior
- [ ] T047 Run tests with `cargo test test_handle_keeps_alive test_clone_independent_lifetime test_drop_from_foreign_thread test_origin_thread_exit -- --test-threads=1`

---

## Phase 5: [US3] Weak Cross-Thread References (P2)

**Goal**: Implement WeakCrossThreadHandle type that doesn't prevent collection

**Independent Test Criteria**: Weak handles don't prevent collection; liveness checks work correctly

### Weak Handle Type

- [ ] T050 Define `WeakCrossThreadHandle<T: Trace + 'static>` struct in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`:
  ```rust
  pub struct WeakCrossThreadHandle<T: Trace + 'static> {
      weak: GcBoxWeakRef<T>,
      origin_tcb: Arc<ThreadControlBlock>,
      origin_thread: ThreadId,
  }
  ```

### Send + Sync for Weak Handle

- [ ] T051 Implement unsafe Send + Sync for WeakCrossThreadHandle<T> in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs` with SAFETY comment

### Weak Handle Methods

- [ ] T052 Implement `origin_thread()` method for WeakCrossThreadHandle
- [ ] T053 Implement `is_valid()` method for WeakCrossThreadHandle (checks via weak.upgrade())
- [ ] T054 Implement `resolve(&self)` for WeakCrossThreadHandle returning Option<Gc<T>>:
  ```rust
  pub fn resolve(&self) -> Option<Gc<T>> {
      assert_eq!(std::thread::current().id(), self.origin_thread,
          "WeakCrossThreadHandle::resolve() must be called on the origin thread");
      self.weak.upgrade()
  }
  ```

### Clone for Weak Handle

- [ ] T055 Implement `Clone` for WeakCrossThreadHandle in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`

### Debug for Weak Handle

- [ ] T056 Implement `Debug` for WeakCrossThreadHandle in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`

### Gc Extension for Weak Handle

- [ ] T057 Add `Gc::weak_cross_thread_handle(&self)` method in `/home/noah/Desktop/rudo/crates/rudo-gc/src/ptr.rs`:
  ```rust
  pub fn weak_cross_thread_handle<T: Trace + 'static>(&self) -> WeakCrossThreadHandle<T> {
      WeakCrossThreadHandle {
          weak: self.as_weak(),
          origin_tcb: current_thread_tcb(),
          origin_thread: std::thread::current().id(),
      }
  }
  ```

### Downgrade Method

- [ ] T058 Add `GcHandle::downgrade(&self)` method in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`:
  ```rust
  pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
      WeakCrossThreadHandle {
          weak: self.ptr.as_ref().as_weak(),
          origin_tcb: Arc::clone(&self.origin_tcb),
          origin_thread: self.origin_thread,
      }
  }
  ```

### Module Exports

- [ ] T059 Export `WeakCrossThreadHandle` from `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/mod.rs`
- [ ] T060 Export `WeakCrossThreadHandle` from `/home/noah/Desktop/rudo/crates/rudo-gc/src/lib.rs`

### US3 Tests

- [ ] T061 Write `test_weak_handle_no_prevent` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create Gc<T>, create weak handle, drop Gc, force GC, verify weak handle's is_valid() returns false
- [ ] T062 Write `test_downgrade` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create strong handle, downgrade to weak, verify weak resolves to live object, drop strong, verify weak no longer resolves
- [ ] T063 Run tests with `cargo test test_weak_handle_no_prevent test_downgrade -- --test-threads=1`

---

## Phase 6: [US4] Defensive Thread Handling (P2)

**Goal**: Implement try_resolve() for graceful handling of uncertain thread context

**Independent Test Criteria**: try_resolve() returns None on wrong thread; defensive patterns work correctly

### Try Resolve Implementation

- [ ] T070 Implement `GcHandle::try_resolve(&self)` in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`:
  ```rust
  pub fn try_resolve(&self) -> Option<Gc<T>> {
      if std::thread::current().id() != self.origin_thread {
          return None;
      }
      Some(unsafe { Gc::from_raw(self.ptr) })
  }
  ```
- [ ] T071 Implement `WeakCrossThreadHandle::try_resolve(&self)` in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`:
  ```rust
  pub fn try_resolve(&self) -> Option<Gc<T>> {
      if std::thread::current().id() != self.origin_thread {
          return None;
      }
      self.weak.upgrade()
  }
  ```

### Unregister Idempotency

- [ ] T072 Verify `unregister()` is idempotent (calling twice is safe)

### US4 Tests

- [ ] T073 Write `test_try_resolve_wrong_thread` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Create handle, spawn foreign thread, verify try_resolve() returns None (no panic)
- [ ] T074 Write `test_unregister_idempotent` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Call unregister() twice, verify second call doesn't panic, verify resolve() panics
- [ ] T075 Write `test_is_valid_checks` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Test is_valid() returns true when registered, false after unregister
- [ ] T076 Run tests with `cargo test test_try_resolve_wrong_thread test_unregister_idempotent test_is_valid_checks -- --test-threads=1`

---

## Phase 7: Testing Strategy

**Goal**: Complete all integration tests and verify memory safety

### Comprehensive Tests

- [ ] T080 Write `test_miri_thread_safety` in `/home/noah/Desktop/rudo/tests/cross_thread_handle.rs`: Run Miri verification for unsafe code paths
- [ ] T081 Write doc tests in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs` for GcHandle demonstrating usage patterns
- [ ] T082 Write doc tests in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs` for WeakCrossThreadHandle demonstrating usage patterns
- [ ] T083 Write doc tests in `/home/noah/Desktop/rudo/crates/rudo-gc/src/ptr.rs` for cross_thread_handle() and weak_cross_thread_handle()

### Full Test Suite

- [ ] T084 Run `./test.sh` to execute all tests including ignored
- [ ] T085 Run `./miri-test.sh` to verify unsafe code safety
- [ ] T086 Run `./clippy.sh` to ensure zero warnings
- [ ] T087 Run `cargo fmt --all` and commit any formatting changes

---

## Phase 8: Polish & Documentation

**Goal**: Complete documentation and polish public API

### Documentation

- [ ] T090 Add comprehensive API documentation comments to all public types and methods in `/home/noah/Desktop/rudo/crates/rudo-gc/src/handles/cross_thread.rs`
- [ ] T091 Add comprehensive API documentation comments to extension methods in `/home/noah/Desktop/rudo/crates/rudo-gc/src/ptr.rs`
- [ ] T092 Update `/home/noah/Desktop/rudo/AGENTS.md` with new feature entry (012-cross-thread-gchandle)
- [ ] T093 Add examples from quickstart.md as doc tests where appropriate

### Final Verification

- [ ] T094 Run full test suite one final time
- [ ] T095 Verify all acceptance scenarios from spec.md are covered by tests
- [ ] T096 Create commit with all changes

---

## Implementation Strategy

### MVP Scope (Phase 3)

The MVP for this feature is User Story 1: Async UI Update Scheduling. This includes:
- Core GcHandle struct
- Send + Sync implementation
- cross_thread_handle() extension method
- Basic resolve() functionality
- test_cross_thread_send, test_resolve_origin_thread

This is sufficient to demonstrate the primary value proposition: sending GC references between threads.

### Incremental Delivery

| Phase | Deliverable | User Stories Enabled |
|-------|-------------|---------------------|
| Phase 3 | GcHandle core + resolve | US1 |
| Phase 4 | GC integration + lifetime management | US1 (complete), US2 |
| Phase 5 | WeakCrossThreadHandle | US3 |
| Phase 6 | try_resolve + defensive patterns | US4 |
| Phase 7-8 | Tests + documentation | All stories complete |

---

## Task Summary

| Phase | Task Count | Description |
|-------|------------|-------------|
| Phase 1: Setup | 6 tasks | Project initialization |
| Phase 2: Foundational | 6 tasks | Core infrastructure (ThreadControlBlock, HandleId) |
| Phase 3: [US1] | 15 tasks | Core GcHandle type + tests |
| Phase 4: [US2] | 8 tasks | GC integration + lifetime tests |
| Phase 5: [US3] | 14 tasks | WeakCrossThreadHandle + tests |
| Phase 6: [US4] | 7 tasks | try_resolve + defensive tests |
| Phase 7: Testing | 9 tasks | Miri, doc tests, full suite |
| Phase 8: Polish | 7 tasks | Documentation, final verification |
| **Total** | **72 tasks** | |

**Parallel Opportunities**: 2 tasks marked [P] (T020, T021, T022, T023, T024 can execute in parallel once T019 dependencies are complete)

**Suggested Starting Point**: Phase 3 Task T020 - Define GcHandle struct (core of the feature)

---

## Running Tests

All tests must use `--test-threads=1` to avoid GC interference:

```bash
# Run specific test
cargo test test_name -- --test-threads=1

# Run full test suite
./test.sh

# Run Miri tests
./miri-test.sh

# Run clippy
./clippy.sh
```

---

## References

- Implementation Plan: [plan.md](plan.md)
- Feature Specification: [spec.md](spec.md)
- Quickstart Guide: [quickstart.md](quickstart.md)
- Constitution: `/home/noah/Desktop/rudo/.specify/memory/constitution.md`
- AGENTS.md: `/home/noah/Desktop/rudo/AGENTS.md`
