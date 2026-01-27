# Implementation Plan: Chez Scheme GC optimizations for rudo-gc

**Branch**: `001-chez-gc-optimization` | **Date**: 2026-01-27 | **Spec**: [link](spec.md)
**Input**: Feature specification from `/specs/001-chez-gc-optimization/spec.md`

## Summary

This plan implements five Chez Scheme-inspired optimizations for rudo-gc to reduce GC pause times and improve memory efficiency in multi-threaded applications. The optimizations are: (1) push-based work transfer to reduce steal contention, (2) segment ownership for better load distribution, (3) mark bitmap to replace forwarding pointers, (4) lock ordering enforcement to prevent deadlocks, and (5) dynamic stack growth monitoring. All optimizations are based on patterns observed in the Chez Scheme garbage collector at `/learn-projects/ChezScheme/c/`.

## Technical Context

**Language/Version**: Rust 1.75+ (as specified in AGENTS.md)
**Primary Dependencies**: `std::sync::atomic`, `std::sync::Mutex`, `std::thread`, `std::sync::Barrier` (Rust stdlib only)
**Storage**: In-memory heap (N/A for external storage)
**Testing**: `cargo test --test-threads=1`, `./miri-test.sh` for unsafe code
**Target Platform**: Linux x86_64/aarch64 (primary), macOS, Windows (cross-platform)
**Project Type**: Rust library crate (rudo-gc)
**Performance Goals**: 30% reduction in p95 GC pause time; 50% reduction in per-object memory overhead for small objects
**Constraints**: No external crates; must pass Miri tests; lock ordering discipline must be enforced
**Scale/Scope**: Multi-threaded applications with 2-16 worker threads; optimizations target parallel marking phase

## Constitution Check

### GATE: Must pass before Phase 0 research (Re-checked post-design)

| Requirement | Status | Notes |
|-------------|--------|-------|
| Memory Safety - Unsafe code has SAFETY comments | PASS | All unsafe operations will have documented contracts |
| Testing Discipline - Tests required before merge | PASS | Integration tests planned for collection correctness |
| Performance-First - O(1) allocation maintained | PASS | BiBOP layout preserved; optimizations target marking phase |
| API Consistency - Rust naming conventions | PASS | Will follow `snake_case` functions, `PascalCase` types |
| Cross-Platform - x86_64/aarch64 support | PASS | Platform-specific code clearly marked |

**Post-Design Validation**: All requirements remain satisfied after Phase 1 design. No new unsafe code patterns introduced beyond documented contracts. Performance targets aligned with constitution metrics.

## Project Structure

### Documentation (this feature)

```text
specs/001-chez-gc-optimization/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
└── checklists/
    └── requirements.md  # Specification quality checklist
```

### Source Code (repository root)

```text
rudo-gc/src/
├── heap/
│   ├── mark/
│   │   ├── bitmap.rs          # NEW: MarkBitmap implementation
│   │   ├── queue.rs           # MODIFIED: PerThreadMarkQueue with push-based transfer
│   │   └── ownership.rs       # NEW: Segment ownership integration
│   ├── sync.rs                # MODIFIED: Lock ordering discipline
│   └── page.rs                # MODIFIED: PageHeader with bitmap support
├── marker.rs                  # MODIFIED: Parallel marking coordinator
├── worklist.rs                # MODIFIED: Chase-Lev deque with ownership
└── gc.rs                      # MODIFIED: GC orchestrator

tests/
├── integration/
│   ├── parallel_marking.rs    # NEW: Multi-threaded marking tests
│   └── work_stealing.rs       # NEW: Push-based transfer tests
└── benchmarks/
    └── marking.rs             # NEW: GC pause time benchmarks
```

**Structure Decision**: Rust library crate structure with feature modules under `src/heap/mark/`. Tests separated into integration tests for multi-threaded scenarios and benchmarks for performance validation.

## Complexity Tracking

> Not applicable - no constitution violations requiring justification.
