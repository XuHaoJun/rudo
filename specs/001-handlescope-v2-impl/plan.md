# Implementation Plan: HandleScope v2 Implementation

**Branch**: `001-handlescope-v2-impl` | **Date**: 2026-02-01 | **Spec**: [link](../spec.md)
**Input**: Feature specification from `/specs/001-handlescope-v2-impl/spec.md`

## Summary

Implement HandleScope v2 for rudo-gc, providing compile-time lifetime-bound handles that replace the v1 conservative root scanning approach. The implementation includes:

1. **HandleScope<'env>**: RAII-style scope management with automatic handle invalidation
2. **Handle<'scope, T>**: Lifetime-bound GC references preventing dangling handles at compile time
3. **EscapeableHandleScope<'env>**: Controlled handle escape to parent scope (single use)
4. **SealedHandleScope<'env>**: Debug-only mechanism to prevent handle creation
5. **AsyncHandleScope**: Async/await-safe scope using Arc<ThreadControlBlock>
6. **AsyncHandle<T>**: Handle without lifetime parameter for async contexts
7. **spawn_with_gc! macro**: Ergonomic wrapper for tokio::spawn with automatic root tracking

The implementation follows V8's HandleScope design patterns while leveraging Rust's type system for compile-time safety.

## Technical Context

**Language/Version**: Rust 1.75+ (stable, with `std::sync::atomic` features)
**Primary Dependencies**: std library only (no external crates for core functionality), tokio for async support
**Storage**: N/A (in-memory garbage collector, heap managed internally)
**Testing**: cargo test, Miri for unsafe code validation, --test-threads=1 for GC tests
**Target Platform**: Cross-platform (x86_64/aarch64 Linux/macOS/Windows)
**Project Type**: Rust library crate (`crates/rudo-gc`)
**Performance Goals**: O(1) handle allocation, zero-overhead abstraction in release builds
**Constraints**: Must maintain soundness, no undefined behavior, interior pointer support already implemented
**Scale/Scope**: Core GC infrastructure affecting all rudo-gc users

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

### Memory Safety (NON-NEGOTIABLE)
- ✅ All unsafe code will have explicit SAFETY comments
- ✅ Miri tests will validate unsafe operations
- ✅ Handle lifetime binding prevents dangling handles at compile time
- ✅ PhantomData correctly conveys ownership semantics
- ✅ GC will never access freed memory

### Testing Discipline (NON-NEGOTIABLE)
- ✅ Unit tests for each component (HandleScope, Handle, AsyncHandleScope, etc.)
- ✅ Integration tests for GC root collection
- ✅ Miri tests for unsafe pointer operations
- ✅ GC interference tests with --test-threads=1
- ✅ Tests for escape patterns, async handling, edge cases

### Performance-First Design
- ✅ O(1) bump allocation for handles
- ✅ BiBOP layout maintained for GC heap
- ✅ No heap allocation for scope management
- ✅ HandleBlock size of 256 slots is cache-friendly

### API Consistency
- ✅ Follows Rust naming conventions (snake_case, PascalCase)
- ✅ Result<T, E> for recoverable errors
- ✅ panic! for programmer errors (e.g., double escape)
- ✅ Doc comments with examples for all public APIs

### Cross-Platform Reliability
- ✅ Uses only std library primitives (AtomicUsize, AtomicBool, Mutex, etc.)
- ✅ No platform-specific code in core handle implementation
- ✅ Consistent behavior across x86_64 and aarch64

## Project Structure

### Documentation (this feature)

```text
specs/001-handlescope-v2-impl/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   └── api.md           # Public API contracts
└── tasks.md             # Phase 2 output (/speckit.tasks command)
```

### Source Code (repository root)

```text
crates/rudo-gc/src/
├── handles/             # NEW: HandleScope implementation
│   ├── mod.rs           # Main module with HandleScope, Handle, etc.
│   ├── local_handles.rs # LocalHandles, HandleBlock, HandleSlot
│   ├── async.rs         # AsyncHandleScope, AsyncHandle, spawn_with_gc!
│   └── tests/           # Unit tests
├── lib.rs               # Updated exports
├── heap.rs              # Updated GC integration
├── gc.rs                # Updated root collection
└── tests/
    ├── handlescope_basic.rs      # Basic HandleScope tests
    ├── handlescope_escape.rs     # Escape pattern tests
    ├── handlescope_async.rs      # AsyncHandleScope tests
    ├── handlescope_thread.rs     # Thread safety tests
    └── handlescope_integration.rs # GC integration tests

crates/rudo-gc/Cargo.toml
```

**Structure Decision**: Add new `handles/` module with submodules for core types, async support, and tests. Follows existing rudo-gc project structure.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Raw pointers in Handle | Zero-cost abstraction requires direct slot access; PhantomData ensures safety | Safe abstraction would add vtable/indirection overhead |
| Cell<bool> in EscapeableHandleScope | Tracking escape state without borrowing issues | Could use AtomicBool but Cell is sufficient (single-threaded) |
| *mut HandleSlot in scope_data | Bump pointer allocation pattern requires mutable pointer | UnsafeCell would add per-allocation overhead |

---

## Phase 1 Re-evaluation: Constitution Check

*Re-checked after completing Phase 0 research and Phase 1 design*

### Memory Safety (NON-NEGOTIABLE) - PASS ✅
- All unsafe operations documented with SAFETY comments in research.md
- Miri test scenarios identified for each unsafe operation
- Lifetime binding via `Handle<'scope, T>` prevents dangling handles at compile time
- PhantomData correctly conveys ownership: `(&'scope (), *const T)`

### Testing Discipline (NON-NEGOTIABLE) - PASS ✅
- Unit tests identified for each component
- Integration tests planned for GC root collection
- Miri test scenarios documented in research.md Section 5.3
- GC interference tests with --test-threads=1 required

### Performance-First Design - PASS ✅
- O(1) bump allocation confirmed
- HandleBlock size of 256 slots is cache-friendly
- Zero heap allocation for scope management
- Raw pointers used only where necessary for zero-cost abstraction

### API Consistency - PASS ✅
- Rust naming conventions followed (snake_case, PascalCase)
- Error handling: panic! for programmer errors (double escape, etc.)
- Doc comments with examples planned for all public APIs
- Comprehensive API contracts documented in contracts/api.md

### Cross-Platform Reliability - PASS ✅
- Uses only std library primitives (AtomicUsize, AtomicBool, Mutex, etc.)
- No platform-specific code in core implementation
- Consistent behavior guaranteed across x86_64 and aarch64

---

## Phase 0: Research Summary

Key research findings from V8 HandleScope implementation and rudo-gc architecture:

### V8 HandleScope Patterns
- HandleScopeData: next, limit, level (no is_escapeable - it's a scope type property)
- LocalHandles: linked list of HandleBlocks for O(1) allocation
- Handle: single word in release (pointer to GcBox)
- EscapableHandleScope: pre-allocates escape slot in parent scope

### rudo-gc Integration Points
- ThreadControlBlock already exists; needs local_handles and async_scopes fields
- GC root collection needs to call iterate_all_handles() during marking
- Interior pointer support already in find_gc_box_from_ptr (verified)

### Key Design Decisions
1. HandleBlock size: 256 slots (V8 uses ~1KB blocks, 256 is reasonable for Rust)
2. Atomic ordering: Relaxed for allocation counter, Acquire/Release for scope lifecycle
3. No Send/Sync for Handle<'scope, T> (thread-local by design)
4. AsyncHandleScope uses Arc<TCB> for cross-await persistence
