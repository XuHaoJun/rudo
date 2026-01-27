# Implementation Plan: Send + Sync Trait Support

**Branch**: `002-send-sync-trait` | **Date**: 2026-01-27 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `/specs/002-send-sync-trait/spec.md`

---

## Summary

Implement `Send` and `Sync` traits for `Gc<T>` and `Weak<T>` smart pointers in rudo-gc, enabling multi-threaded garbage-collected pointer sharing. The implementation replaces non-atomic `Cell` types with atomic `AtomicUsize` for reference counting and `AtomicPtr` for pointer storage, following memory ordering patterns from ChezScheme. This establishes the foundation for future parallel marking while maintaining backward compatibility for single-threaded usage.

---

## Technical Context

**Language/Version**: Rust 1.75+  
**Primary Dependencies**: `std::sync::atomic` (Rust stdlib), no external crates  
**Storage**: N/A (in-memory garbage collector, heap managed internally)  
**Testing**: `cargo test`, `./test.sh`, `./miri-test.sh`, ThreadSanitizer  
**Target Platform**: x86_64 and aarch64 on Linux, macOS, Windows  
**Project Type**: Rust library (crate: `rudo-gc`)  
**Performance Goals**: Reference count operations < 100ns, zero data races under concurrent load  
**Constraints**: Must pass all Miri tests, must maintain O(1) allocation, parallel marking out of scope  
**Scale/Scope**: Modifies `GcBox<T>` and `Gc<T>` in `ptr.rs` only; changes affect 2 core types

---

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

### Memory Safety (NON-NEGOTIABLE)

| Requirement | Status | Evidence |
|-------------|--------|----------|
| All unsafe code has SAFETY comments | Required | Will add to `ptr.rs` modifications |
| Miri tests pass for atomic ref operations | Required | `./miri-test.sh` in CI |
| GcBox maintains ownership semantics | Required | Atomic operations preserve invariants |

**Verdict**: ✅ PASS - Implementation approach uses standard atomic operations with SAFETY comments

### Testing Discipline (NON-NEGOTIABLE)

| Requirement | Status | Evidence |
|-------------|--------|----------|
| All new features have tests | Required | Will add unit tests in `ptr.rs` |
| GC interference tests use `--test-threads=1` | Required | Existing test configuration |
| Integration tests for cross-thread behavior | Required | Will add parallel stress tests |

**Verdict**: ✅ PASS - Testing strategy aligned with constitution

### Performance-First Design

| Requirement | Status | Evidence |
|-------------|--------|----------|
| Allocation remains O(1) | Required | BiBOP layout unchanged |
| Memory overhead bounded | Required | AtomicUsize replaces Cell<NonZeroUsize> |

**Verdict**: ✅ PASS - BiBOP architecture unchanged, atomic types same size or smaller

### API Consistency

| Requirement | Status | Evidence |
|-------------|--------|----------|
| Follows Rust naming conventions | Required | `snake_case` for methods |
| Doc comments with examples | Required | Will update documentation |

**Verdict**: ✅ PASS - API additions follow existing patterns

### Cross-Platform Reliability

| Requirement | Status | Evidence |
|-------------|--------|----------|
| Works on x86_64 and aarch64 | Required | `std::sync::atomic` is portable |
| Tests pass on all platforms | Required | CI pipeline verification |

**Verdict**: ✅ PASS - Rust atomic types handle platform differences

**Overall Gate Verdict**: ✅ PASS - All constitution requirements satisfied

---

## Project Structure

### Documentation (this feature)

```text
specs/002-send-sync-trait/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── spec.md              # Feature specification
└── checklists/
    └── requirements.md  # Quality checklist
```

### Source Code (rudo-gc crate)

```text
crates/rudo-gc/src/
├── ptr.rs               # Gc<T>, Weak<T>, GcBox<T> - MODIFIED
├── heap.rs              # BiBOP memory management - REVIEW
├── gc.rs                # Collection algorithm - NO CHANGE
├── trace.rs             # Trace trait - NO CHANGE
└── lib.rs               # Public exports - NO CHANGE

tests/
└── sync/                # New directory for parallel tests
    └── send_sync_tests.rs
```

**Structure Decision**: Changes are isolated to `ptr.rs` with minimal impact on other modules. New test file in `tests/sync/` for parallel/concurrent test coverage.

---

## Phase 0: Research Summary

### Key Findings

**Decision**: Use `AtomicUsize` for reference counts and `AtomicPtr` for pointer storage

**Rationale**: 
- `AtomicUsize` provides platform-independent atomic operations with proper memory ordering
- Matches the pattern used by `dumpster` (reference implementation)
- Rust's std atomic types handle platform-specific differences (x86 vs ARM)

**Alternatives Considered**:
- `crossbeam::AtomicCell`: Additional dependency, not needed for this feature
- `parking_lot::Mutex<Gc<T>>`: Overkill, adds synchronization overhead
- Custom CAS loop: Error-prone, less maintainable

### Memory Ordering Strategy (from ChezScheme)

| Operation | Ordering | Rationale |
|-----------|----------|-----------|
| `inc_ref` | `Relaxed` | Count only, no synchronization needed |
| `dec_ref` | `AcqRel` | Must synchronize for proper memory release |
| `inc_weak` | `Relaxed` | Weak count is advisory only |
| `dec_weak` | `AcqRel` | Must synchronize weak count changes |
| Pointer load | `Acquire` | Must see fully initialized object |
| Pointer store | `Release` | Must make initialization visible |

### Reference Implementation Analysis

**dumpster (learn-projects/dumpster/dumpster/src/sync/mod.rs)**:
- Implements `unsafe impl<T> Send for Gc<T> where T: Trace + Send + Sync + ?Sized`
- Uses `AtomicUsize` for `GcBox.strong` and `GcBox.weak`
- `UCell<Nullable<...>>` for atomic pointer storage

**ChezScheme (learn-projects/ChezScheme/c/atomic.h)**:
- Platform-specific memory barriers (x86_64 vs ARM64)
- Uses `COMPARE_AND_SWAP_PTR` macro for atomic operations
- Mark bits use separate atomic operations

### Implementation Notes

- Reference count saturates at `isize::MAX` to prevent overflow
- CAS loop with exponential backoff on contention for `dec_ref`
- All unsafe code will have explicit SAFETY comments per constitution

---

## Phase 1: Design Artifacts

### Data Model

```text
GcBox<T: Trace + ?Sized>
├── ref_count: AtomicUsize     (strong reference count)
├── weak_count: AtomicUsize    (weak reference count)
├── drop_fn: unsafe fn(*mut u8) (type-erased destructor)
├── trace_fn: unsafe fn(*const u8, &mut GcVisitor) (type-erased tracer)
└── value: T                   (user data)

Gc<T: Trace + ?Sized + 'static>
└── ptr: AtomicPtr<GcBox<T>>   (atomic pointer to GcBox)

Weak<T: Trace + ?Sized + 'static>
└── ptr: AtomicPtr<GcBox<T>>   (atomic pointer to GcBox)
```

### Trait Implementations (New)

```rust
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for Gc<T> {}

unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for Gc<T> {}

unsafe impl<T: Trace + Send + Sync + ?Sized> Send for Weak<T> {}

unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for Weak<T> {}
```

### API Changes

| Component | Change | Impact |
|-----------|--------|--------|
| `GcBox::ref_count` | `Cell<NonZeroUsize>` → `AtomicUsize` | Breaking internal change |
| `GcBox::weak_count` | `Cell<usize>` → `AtomicUsize` | Breaking internal change |
| `Gc::ptr` | `Cell<Nullable<GcBox<T>>>` → `AtomicPtr<GcBox<T>>` | Breaking internal change |
| `Gc` trait bounds | Add `Send + Sync` conditional | New capability |
| `Weak` trait bounds | Add `Send + Sync` conditional | New capability |

### Backward Compatibility

- Existing single-threaded `Gc<T>` usage continues to work unchanged
- New `Send + Sync` capability is additive, not breaking
- Internal representation changes are hidden behind public API

---

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No constitution violations require justification. The implementation follows all principles:

- Memory safety: Achieved through atomic operations and SAFETY comments
- Testing: Comprehensive test coverage planned
- Performance: BiBOP layout unchanged, atomic overhead acceptable
- API: Consistent with existing rudo-gc patterns
- Cross-platform: Uses portable Rust atomic types

---

## Generated Artifacts

| Artifact | Status | Path |
|----------|--------|------|
| research.md | Complete | `specs/002-send-sync-trait/research.md` |
| data-model.md | Complete | `specs/002-send-sync-trait/data-model.md` |
| quickstart.md | Complete | `specs/002-send-sync-trait/quickstart.md` |

---

## Next Steps

Proceed to `/speckit.tasks` to generate implementation tasks based on this plan.

**Recommended Tasks Order**:
1. Modify `GcBox<T>` to use atomic types
2. Modify `Gc<T>` and `Weak<T>` pointer storage
3. Add `Send + Sync` trait implementations
4. Add SAFETY comments to all unsafe code
5. Add unit tests for atomic operations
6. Add integration tests for multi-threaded usage
7. Run full test suite (including Miri)
