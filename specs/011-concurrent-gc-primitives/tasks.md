# Tasks: Concurrent GC Primitives

**Feature**: 011-concurrent-gc-primitives | **Generated**: 2026-02-08

## Overview

Implement thread-safe concurrent GC primitives (`GcRwLock` and `GcMutex`) using `parking_lot` synchronization primitives, enabling garbage-collected objects to be safely shared across threads while bypassing locks during GC STW pauses.

## Dependencies

**Story Completion Order:**
- US1 (Multi-threaded GC Object Sharing) ← Foundation for all concurrent usage
- US2 (Performance Isolation) ← Independent; GcCell unchanged
- US3 (GC Safety During Mark Phase) ← Requires US1 implementation
- US4 (Write Barrier Integration) ← Requires US1

**Recommended Execution Order:**
1. Phase 1-2 (Setup & Foundational)
2. US1 (Priority P1 - core feature)
3. US3 (depends on US1 Trace bypass)
4. US4 (depends on US1 guard types)
5. US2 (independent; can be verified anytime)
6. Final Phase (Polish)

---

## Phase 1: Setup

Project initialization and dependency configuration.

- [X] T001 Add `parking_lot` dependency to `crates/rudo-gc/Cargo.toml`
- [X] T002 Create `crates/rudo-gc/src/sync.rs` module with basic module declaration
- [X] T003 Add `pub mod sync;` to `crates/rudo-gc/src/lib.rs`
- [X] T004 Add `pub use sync::{GcRwLock, GcMutex};` re-exports to `crates/rudo-gc/src/lib.rs`

---

## Phase 2: Foundational Types

Core type definitions and Trace implementations for lock bypass.

- [X] T005 Define `GcRwLock<T>` struct with `inner: parking_lot::RwLock<T>` field in `crates/rudo-gc/src/sync.rs`
- [X] T006 Define `GcMutex<T>` struct with `inner: parking_lot::Mutex<T>` field in `crates/rudo-gc/src/sync.rs`
- [X] T007 Implement `Default` trait for `GcRwLock<T>` where `T: Default` in `crates/rudo-gc/src/sync.rs`
- [X] T008 Implement `Default` trait for `GcMutex<T>` where `T: Default` in `crates/rudo-gc/src/sync.rs`
- [X] T009 Implement `Debug` trait for `GcRwLock<T>` where `T: Debug` in `crates/rudo-gc/src/sync.rs`
- [X] T010 Implement `Debug` trait for `GcMutex<T>` where `T: Debug` in `crates/rudo-gc/src/sync.rs`
- [ ] T011 Implement `Clone` trait for `GcRwLock<T>` where `T: Clone` in `crates/rudo-gc/src/sync.rs`
- [ ] T012 Implement `Clone` trait for `GcMutex<T>` where `T: Clone` in `crates/rudo-gc/src/sync.rs`

---

## Phase 3: User Story 1 - Multi-threaded GC Object Sharing

**Goal**: Enable safe sharing of GC objects between threads using GcRwLock and GcMutex.

**Independent Test Criteria**: Create a shared GC object protected by GcRwLock/GcMutex, access it from multiple threads concurrently, verify no data races or memory errors.

**Acceptance Scenarios**:
1. Multiple reader threads calling `read()` concurrently access data safely
2. Multiple threads competing for GcMutex lock with proper exclusion
3. Writers proceed after readers release, waiting readers/writers proceed after write

### Implementation Tasks

- [X] T013 [P] [US1] Implement `GcRwLock::new(value: T) -> GcRwLock<T>` constructor in `crates/rudo-gc/src/sync.rs`
- [X] T014 [P] [US1] Implement `GcRwLock<T>::read(&self) -> GcRwLockReadGuard<'_, T>` method in `crates/rudo-gc/src/sync.rs`
- [X] T015 [P] [US1] Implement `GcRwLock<T>::try_read(&self) -> Option<GcRwLockReadGuard<'_, T>>` method in `crates/rudo-gc/src/sync.rs`
- [X] T016 [P] [US1] Implement `GcRwLock<T>::write(&self) -> GcRwLockWriteGuard<'_, T>` method in `crates/rudo-gc/src/sync.rs`
- [X] T017 [P] [US1] Implement `GcRwLock<T>::try_write(&self) -> Option<GcRwLockWriteGuard<'_, T>>` method in `crates/rudo-gc/src/sync.rs`
- [X] T018 [P] [US1] Implement `GcRwLock<T>::is_locked(&self) -> bool` method in `crates/rudo-gc/src/sync.rs`
- [X] T019 [P] [US1] Define `GcRwLockReadGuard<'a, T>` struct with lifetime tied to parent in `crates/rudo-gc/src/sync.rs`
- [X] T020 [P] [US1] Implement `Deref` trait for `GcRwLockReadGuard<T>` dereferencing to `&T` in `crates/rudo-gc/src/sync.rs`
- [X] T021 [P] [US1] Implement `Drop` trait for `GcRwLockReadGuard<T>` releasing read lock on drop in `crates/rudo-gc/src/sync.rs`
- [X] T022 [P] [US1] Define `GcRwLockWriteGuard<'a, T>` struct with lifetime tied to parent in `crates/rudo-gc/src/sync.rs`
- [X] T023 [P] [US1] Implement `Deref` trait for `GcRwLockWriteGuard<T>` dereferencing to `&mut T` in `crates/rudo-gc/src/sync.rs`
- [X] T024 [P] [US1] Implement `DerefMut` trait for `GcRwLockWriteGuard<T>` in `crates/rudo-gc/src/sync.rs`
- [X] T025 [P] [US1] Implement `Drop` trait for `GcRwLockWriteGuard<T>` releasing write lock on drop in `crates/rudo-gc/src/sync.rs`
- [X] T026 [US1] Implement `GcMutex<T>::new(value: T) -> GcMutex<T>` constructor in `crates/rudo-gc/src/sync.rs`
- [X] T027 [US1] Implement `GcMutex<T>::lock(&self) -> GcMutexGuard<'_, T>` method in `crates/rudo-gc/src/sync.rs`
- [X] T028 [US1] Implement `GcMutex<T>::try_lock(&self) -> Option<GcMutexGuard<'_, T>>` method in `crates/rudo-gc/src/sync.rs`
- [X] T029 [US1] Implement `GcMutex<T>::is_locked(&self) -> bool` method in `crates/rudo-gc/src/sync.rs`
- [X] T030 [US1] Define `GcMutexGuard<'a, T>` struct with lifetime tied to parent in `crates/rudo-gc/src/sync.rs`
- [X] T031 [US1] Implement `Deref` trait for `GcMutexGuard<T>` dereferencing to `&mut T` in `crates/rudo-gc/src/sync.rs`
- [X] T032 [US1] Implement `DerefMut` trait for `GcMutexGuard<T>` in `crates/rudo-gc/src/sync.rs`
- [X] T033 [US1] Implement `Drop` trait for `GcMutexGuard<T>` releasing mutex on drop in `crates/rudo-gc/src/sync.rs`

### Parallel Opportunities in US1

Tasks T013-T025 (GcRwLock implementation) can execute in parallel with T026-T033 (GcMutex implementation) as they operate on independent types.

---

## Phase 4: User Story 2 - Performance Isolation for Single-Threaded Users

**Goal**: Maintain GcCell unchanged for existing single-threaded workloads without synchronization overhead.

**Independent Test Criteria**: Compare GcCell performance in single-threaded scenarios, verify no atomic operation overhead introduced.

**Acceptance Scenarios**:
1. Existing GcCell code works without modification
2. Single-threaded benchmark performance matches or exceeds previous implementation

### Implementation Tasks

- [ ] T034 [US2] Verify GcCell implementation in `crates/rudo-gc/src/cell.rs` is unchanged (no Sync bounds added)
- [ ] T035 [US2] Add unit tests for GcCell in `crates/rudo-gc/src/cell.rs` to verify no atomic overhead regression
- [ ] T036 [US2] Run benchmarks comparing GcCell performance before/after concurrent primitives implementation

---

## Phase 5: User Story 3 - GC Safety During Mark Phase

**Goal**: Enable GC to trace objects during STW pause without deadlocking on user locks.

**Independent Test Criteria**: Create concurrent GC operations under heavy lock contention, verify GC completes without deadlock.

**Acceptance Scenarios**:
1. GC traces objects by bypassing locks during STW pause
2. GC reads consistent atomic pointer values for paused threads

### Implementation Tasks

- [X] T037 [P] [US3] Implement unsafe `Trace` trait for `GcRwLock<T>` with lock bypass in `crates/rudo-gc/src/sync.rs`
- [X] T038 [P] [US3] Implement unsafe `Trace` trait for `GcMutex<T>` with lock bypass in `crates/rudo-gc/src/sync.rs`
- [X] T039 [US3] Add SAFETY comment for GcRwLock Trace implementation explaining STW guarantee in `crates/rudo-gc/src/sync.rs`
- [X] T040 [US3] Add SAFETY comment for GcMutex Trace implementation explaining STW guarantee in `crates/rudo-gc/src/sync.rs`

### Parallel Opportunities in US3

Tasks T037 and T038 can execute in parallel as they implement the same pattern for independent types.

---

## Phase 6: User Story 4 - Write Barrier Integration

**Goal**: Automatically trigger write barriers when modifying protected GC objects for incremental GC features.

**Independent Test Criteria**: Modify GC objects through guards, verify dirty pages/lists are updated correctly.

**Acceptance Scenarios**:
1. GcRwLock write guard marks page as dirty (generational barrier)
2. SATB barriers record old value when incremental GC is enabled

### Implementation Tasks

- [ ] T041 [P] [US4] Integrate generational barrier trigger in `GcRwLock<T>::write()` method in `crates/rudo-gc/src/sync.rs`
- [ ] T042 [P] [US4] Integrate SATB barrier trigger in `GcRwLock<T>::write()` method in `crates/rudo-gc/src/sync.rs`
- [ ] T043 [P] [US4] Integrate generational barrier trigger in `GcMutex<T>::lock()` method in `crates/rudo-gc/src/sync.rs`
- [ ] T044 [P] [US4] Integrate SATB barrier trigger in `GcMutex<T>::lock()` method in `crates/rudo-gc/src/sync.rs`

### Parallel Opportunities in US4

Tasks T041-T042 (GcRwLock barriers) can execute in parallel with T043-T044 (GcMutex barriers).

---

## Phase 7: Send + Sync Trait Implementation

Ensure proper thread-safety trait bounds.

### Implementation Tasks

- [X] T045 [P] Implement `Send` trait for `GcRwLock<T>` where `T: Trace + Send` in `crates/rudo-gc/src/sync.rs`
- [X] T046 [P] Implement `Sync` trait for `GcRwLock<T>` where `T: Trace + Send + Sync` in `crates/rudo-gc/src/sync.rs`
- [X] T047 [P] Implement `Send` trait for `GcMutex<T>` where `T: Trace + Send` in `crates/rudo-gc/src/sync.rs`
- [X] T048 [P] Implement `Sync` trait for `GcMutex<T>` where `T: Trace + Send + Sync` in `crates/rudo-gc/src/sync.rs`

### Parallel Opportunities

Tasks T045-T046 can execute in parallel with T047-T048.

---

## Phase 8: Polish & Cross-Cutting Concerns

Documentation, integration tests, and verification.

### Unit Tests

- [X] T049 Add unit tests for GcRwLock basic operations in `crates/rudo-gc/src/sync.rs`
- [X] T050 Add unit tests for GcMutex basic operations in `crates/rudo-gc/src/sync.rs`
- [X] T051 Add unit tests for guard drop behavior in `crates/rudo-gc/src/sync.rs`
- [X] T052 Add unit tests for try_read/try_write/try_lock methods in `crates/rudo-gc/src/sync.rs`

### Integration Tests

- [X] T053 Create integration test file `tests/integration_concurrent.rs` for concurrent access scenarios
- [X] T054 Add integration test for multiple readers concurrent access (US1 scenario 1)
- [X] T055 Add integration test for writer priority and lock escalation (US1 scenario 3)
- [X] T056 Add integration test for GcMutex exclusive access (US1 scenario 2)
- [X] T057 Add integration test for GC tracing under lock contention (US3)
- [X] T058 Add integration test for write barrier triggering (US4)

### Miri & ThreadSanitizer Tests

- [ ] T059 Add Miri test for unsafe Trace bypass implementation in `tests/integration_concurrent.rs`
- [ ] T060 Add ThreadSanitizer test for concurrent access data race detection in `tests/integration_concurrent.rs`

### Documentation

- [X] T061 Add module-level documentation comments to `crates/rudo-gc/src/sync.rs`
- [X] T062 Add documentation for GcRwLock type including usage examples in `crates/rudo-gc/src/sync.rs`
- [X] T063 Add documentation for GcMutex type including usage examples in `crates/rudo-gc/src/sync.rs`
- [X] T064 Add documentation for guard types in `crates/rudo-gc/src/sync.rs`

### Verification

- [X] T065 Run `./clippy.sh` and fix all warnings
- [X] T066 Run `./test.sh` and ensure all tests pass
- [X] T067 Run `cargo fmt --all` to format code
- [ ] T068 Run Miri tests for unsafe code verification if applicable

---

## Summary

| Metric | Value |
|--------|-------|
| **Total Tasks** | 68 |
| **Completed** | 55 |
| **Remaining** | 13 |
| **Phase 1 (Setup)** | 4/4 ✓ |
| **Phase 2 (Foundational)** | 6/8 (Clone pending) |
| **Phase 3 (US1 - Multi-threaded Sharing)** | 21/21 ✓ |
| **Phase 4 (US2 - Performance Isolation)** | 0/3 (pending) |
| **Phase 5 (US3 - GC Safety)** | 4/4 ✓ |
| **Phase 6 (US4 - Write Barriers)** | 0/4 (deferred) |
| **Phase 7 (Send + Sync)** | 4/4 ✓ |
| **Phase 8 (Polish)** | 15/20 (Miri tests pending) |

### Tasks per User Story

| User Story | Task Count | Status |
|------------|------------|--------|
| US1 - Multi-threaded GC Object Sharing | 21 | ✓ COMPLETE |
| US2 - Performance Isolation | 3 | Pending |
| US3 - GC Safety During Mark Phase | 4 | ✓ COMPLETE |
| US4 - Write Barrier Integration | 4 | Deferred |

### Parallel Execution Opportunities

1. **Phase 3 (US1)**: GcRwLock impl (T013-T025) || GcMutex impl (T026-T033)
2. **Phase 5 (US3)**: GcRwLock Trace (T037) || GcMutex Trace (T038)
3. **Phase 6 (US4)**: GcRwLock barriers (T041-T042) || GcMutex barriers (T043-T044)
4. **Phase 7**: GcRwLock Send/Sync (T045-T046) || GcMutex Send/Sync (T047-T048)

### Suggested MVP Scope

**Minimum Viable Product**: Complete Phase 1 + Phase 2 + Phase 3 (US1) + Phase 5 (US3) + Tasks T045-T048 (Send/Sync)

This provides:
- Basic GcRwLock and GcMutex with all lock operations
- GC-safe Trace implementations for concurrent primitives
- Proper Send/Sync trait bounds

US2 (Performance Isolation) is verified by ensuring no regression. US4 (Write Barriers) can be added incrementally for incremental GC support.

---

## References

- **Plan**: `/home/noah/Desktop/rudo/specs/011-concurrent-gc-primitives/plan.md`
- **Spec**: `/home/noah/Desktop/rudo/specs/011-concurrent-gc-primitives/spec.md`
- **Data Model**: `/home/noah/Desktop/rudo/specs/011-concurrent-gc-primitives/data-model.md`
- **Research**: `/home/noah/Desktop/rudo/specs/011-concurrent-gc-primitives/research.md`
- **Quickstart**: `/home/noah/Desktop/rudo/specs/011-concurrent-gc-primitives/quickstart.md`
- **API Contracts**: `/home/noah/Desktop/rudo/specs/011-concurrent-gc-primitives/contracts/api.txt`
