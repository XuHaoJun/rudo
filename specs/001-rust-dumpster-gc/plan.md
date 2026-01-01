# Implementation Plan: Rust Dumpster GC - BiBOP & Mark-Sweep Engine

**Branch**: `001-rust-dumpster-gc` | **Date**: 2026-01-02 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/001-rust-dumpster-gc/spec.md`

## Summary

Develop a Rust garbage collection library that provides a `Gc<T>` smart pointer with automatic memory reclamation. The implementation uses **BiBOP (Big Bag of Pages)** memory layout for efficient O(1) allocation via size classes, combined with a **Mark-Sweep** garbage collection algorithm (non-moving) to handle cycles. The API is designed to be ergonomic, similar to `dumpster`, with a `#[derive(Trace)]` macro for custom types.

Key technical insights from reference implementations:
- **From Chez Scheme**: BiBOP segment organization, Page Header with mark bitmap, O(1) interior pointer resolution via page alignment
- **From dumpster**: Reference counting with cycle detection (we will use full tracing instead), `Trace` trait design, Visitor pattern for traversal

## Technical Context

**Language/Version**: Rust 1.75+ (stable preferred, nightly if `coerce_unsized` needed)  
**Primary Dependencies**: `std` (core allocator APIs), `proc-macro2`, `syn`, `quote` (for derive macro)  
**Storage**: N/A (in-memory heap only)  
**Testing**: `cargo test`, Miri for UB detection  
**Target Platform**: Linux/macOS/Windows (any platform with `std` support)  
**Project Type**: Rust crate library (single project, multi-crate workspace)  
**Performance Goals**: O(1) allocation for small objects, O(live_objects) collection time  
**Constraints**: No object movement (address stability for Rust's `&T`), Stop-the-World collection for MVP  
**Scale/Scope**: MVP supports single-threaded GC; concurrent/parallel marking is post-MVP

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Code Quality Excellence | ✅ Pass | Safe Rust API, unsafe internals well-documented |
| II. Rigorous Testing Standards | ✅ Pass | Unit tests for allocator/GC, integration tests for cycle collection, Miri for safety |
| III. Consistent User Experience | ✅ Pass | Ergonomic API (`Gc<T>`, `derive(Trace)`) modeled after `dumpster` |
| IV. Performance & Efficiency by Design | ✅ Pass | BiBOP for O(1) alloc, Mark-Sweep for cycle handling, designed for latency control |

**Constitution Check Result**: PASS - No violations.

## Project Structure

### Documentation (this feature)

```text
specs/001-rust-dumpster-gc/
├── spec.md              # Feature specification
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output (API traits/types)
├── checklists/          # Quality checklists
└── tasks.md             # Phase 2 output (via /speckit.tasks)
```

### Source Code (repository root)

```text
crates/
├── rudo-gc/                  # Main GC library
│   ├── src/
│   │   ├── lib.rs            # Public API (Gc<T>, Trace trait, collect())
│   │   ├── heap.rs           # GlobalHeap and Segment<SIZE> implementation
│   │   ├── gc.rs             # Mark-Sweep algorithm
│   │   ├── trace.rs          # Trace trait and Visitor pattern
│   │   ├── roots.rs          # Root tracking (Shadow Stack or Conservative)
│   │   └── ptr.rs            # Gc<T> smart pointer implementation
│   ├── Cargo.toml
│   └── tests/
│       ├── basic.rs          # Basic allocation/deallocation
│       ├── cycles.rs         # Cycle collection tests
│       └── benchmarks.rs     # Performance tests
│
└── rudo-gc-derive/           # Proc-macro crate
    ├── src/
    │   └── lib.rs            # #[derive(Trace)] implementation
    └── Cargo.toml
```

**Structure Decision**: Multi-crate workspace pattern for Rust library with proc-macro support. The main `rudo-gc` crate provides the GC runtime, while `rudo-gc-derive` provides the derive macro. This mirrors the structure of `dumpster` and `dumpster_derive`.

## Complexity Tracking

> No violations to justify - Constitution Check passed.

## Technical Design Decisions

### 1. Memory Layout: BiBOP (from Chez Scheme / John McCarthy doc)

Each Page (4KB aligned) contains:
- **Page Header**: Magic number, block size, object count, mark bitmap
- **Object Slots**: Fixed-size slots based on size class

Size classes (compile-time determined via `const generics`):
- Class 16: objects ≤16 bytes (max 255 objects per page)
- Class 32: objects ≤32 bytes (max 127 objects per page)
- Class 64: objects ≤64 bytes (max 63 objects per page)
- Class 128, 256, 512, 1024, 2048: larger size classes
- Large Object Space (LOS): objects > 2KB get dedicated pages

### 2. Root Tracking Strategy

Two options were researched (see research.md):
- **Option A (Shadow Stack)**: RAII-based registration of roots. Safer but has runtime overhead.
- **Option B (Conservative Scanning)**: Scan stack memory, treat anything resembling a heap pointer as a root. More complex but zero overhead during normal execution.

**Decision**: Start with Shadow Stack for MVP (safer, easier to implement). Conservative scanning as future optimization.

### 3. Collection Algorithm

1. **Mark Phase**: Starting from roots, traverse object graph using Trace trait. Set bits in page header mark bitmaps.
2. **Sweep Phase**: Iterate all pages, reclaim objects with unmarked bits.
3. **Triggering**: Configurable condition (default: when dropped_count > living_count, similar to dumpster).

### 4. API Design (from dumpster)

```rust
// User-facing types
pub struct Gc<T: Trace + ?Sized>(/* ... */);

pub unsafe trait Trace {
    fn trace(&self, visitor: &mut impl Visitor);
}

pub trait Visitor {
    fn visit<T: Trace + ?Sized>(&mut self, gc: &Gc<T>);
}

// Free functions
pub fn collect();
pub fn set_collect_condition(f: fn(&CollectInfo) -> bool);
```

## Next Steps

1. Create `research.md` documenting technology decisions
2. Create `data-model.md` with key entity definitions
3. Create `contracts/` with trait definitions
4. Create `quickstart.md` with usage examples
5. Run `/speckit.tasks` to generate implementation tasks
