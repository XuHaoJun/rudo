# Implementation Plan: Incremental Marking for Major GC

**Branch**: `008-incremental-marking` | **Date**: 2026-02-03 | **Spec**: [spec.md](./spec.md)  
**Input**: Feature specification from `/specs/008-incremental-marking/spec.md`  
**Reference**: [docs/generational-gc-plan-0.8.md](../../docs/generational-gc-plan-0.8.md)

## Summary

Implement incremental marking to reduce major GC pause times by splitting the mark phase into smaller cooperative increments that interleave with mutator execution. Uses a hybrid SATB (Snapshot-At-The-Beginning) + insertion-barrier approach integrated with the existing dirty page list infrastructure from spec 007. Target: 50-80% reduction in pause times for 1GB+ heaps, with maximum pause under 10ms.

## Technical Context

**Language/Version**: Rust 1.75+ (stable)  
**Primary Dependencies**: `parking_lot` (existing), `crossbeam-queue` (new for lock-free worklist)  
**Storage**: N/A (in-memory GC)  
**Testing**: `cargo test`, Miri for unsafe code, loom for concurrency  
**Target Platform**: x86_64 and aarch64 across Linux, macOS, Windows  
**Project Type**: Library crate (`rudo-gc`)  
**Performance Goals**: Max pause <10ms (1GB heap), mutator utilization >90% during marking  
**Constraints**: Write barrier overhead <10% vs generational GC, total GC time ≤2x STW  
**Scale/Scope**: Heaps 1GB+, multi-threaded applications

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

### I. Memory Safety (NON-NEGOTIABLE) ✅

| Requirement | Plan Compliance |
|-------------|-----------------|
| SAFETY comments for unsafe code | All new unsafe in incremental.rs, gc.rs, cell.rs will have SAFETY comments |
| Miri tests for memory violations | Miri tests for write barrier, mark loop, snapshot scanning |
| No freed memory access | Mark bitmap prevents double-free; SATB prevents lost objects |
| GcBox ownership semantics | No changes to GcBox ownership model |

### II. Testing Discipline (NON-NEGOTIABLE) ✅

| Requirement | Plan Compliance |
|-------------|-----------------|
| Tests before merge | Unit tests for state machine, write barrier, mark loop |
| Miri for unsafe changes | Miri tests for all new unsafe code paths |
| `--test-threads=1` | All GC tests use single-threaded execution |
| Integration tests | Tests for correctness, cycle detection, concurrent mutation |

### III. Performance-First Design ✅

| Requirement | Plan Compliance |
|-------------|-----------------|
| Benchmark detection | Pause time benchmarks comparing STW vs incremental |
| O(1) allocation | BiBOP layout unchanged; new allocations marked immediately |
| Generational hypothesis | Integrates with dirty page list; minor GC blocked during major marking |
| Bounded metadata | Worklist bounded; dirty snapshot bounded with overflow→STW fallback |

### IV. API Consistency ✅

| Requirement | Plan Compliance |
|-------------|-----------------|
| Naming conventions | `IncrementalMarkState`, `MarkPhase`, `mark_increment()` |
| Error handling | Fallback to STW on threshold exceeded (no panic) |
| Standard idioms | Public API mirrors existing `Gc::yield_now()` pattern |

### V. Cross-Platform Reliability ✅

| Requirement | Plan Compliance |
|-------------|-----------------|
| Conservative stack scanning | Root capture unchanged; works on all platforms |
| Consistent behavior | Atomics and mutexes are cross-platform |
| Platform-specific code marked | No new platform-specific code |

## Project Structure

### Documentation (this feature)

```text
specs/008-incremental-marking/
├── spec.md              # Feature specification
├── plan.md              # This file
├── research.md          # Phase 0 output - design decisions
├── data-model.md        # Phase 1 output - state machine and entities
├── contracts/           # Phase 1 output - internal API contracts
│   └── incremental-api.md
├── quickstart.md        # Phase 1 output - integration guide
└── tasks.md             # Phase 2 output (via /speckit.tasks)
```

### Source Code (repository root)

```text
crates/rudo-gc/src/
├── gc/
│   ├── gc.rs            # MODIFIED: Incremental collection entry points
│   ├── incremental.rs   # NEW: Core incremental marking state
│   ├── marker.rs        # MODIFIED: Incremental worker support
│   ├── mod.rs           # MODIFIED: Export incremental module
│   └── worklist.rs      # MODIFIED: Work-stealing for incremental
├── cell.rs              # MODIFIED: Enhanced write barrier
├── heap.rs              # MODIFIED: Thread-local mark queues, remembered buffer
└── lib.rs               # MODIFIED: Gc::yield_now(), IncrementalConfig

crates/rudo-gc/tests/
├── incremental_state.rs      # NEW: State machine tests
├── incremental_write_barrier.rs  # NEW: Barrier correctness tests
├── incremental_marking.rs    # NEW: Mark loop tests
├── incremental_integration.rs    # NEW: Full workflow tests
└── incremental_generational.rs   # NEW: Combined GC tests

crates/rudo-gc/benches/
└── incremental_pause.rs      # NEW: Pause time benchmarks
```

**Structure Decision**: Single library crate with new `gc/incremental.rs` module. Tests in `tests/` directory following existing patterns.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| None identified | N/A | N/A |

---

## Design Decisions Summary

See [research.md](./research.md) for detailed rationale.

### Key Decisions

1. **Algorithm**: Hybrid SATB + Dijkstra insertion barrier (mark new values immediately)
2. **Write Barrier**: Fast path + per-thread remembered buffer (ChezScheme pattern)
3. **Work Distribution**: Per-worker budgets with slice barrier for coordination
4. **Dirty Page Integration**: Reuse spec 007 infrastructure; snapshot dirty pages during increments
5. **Fallback**: STW completion when dirty pages exceed threshold or timeout
6. **New Allocations**: Marked immediately (black) during incremental marking phase

### Phase 1 Artifacts

- [data-model.md](./data-model.md) - State machine and entity definitions
- [contracts/incremental-api.md](./contracts/incremental-api.md) - Internal API contracts
- [quickstart.md](./quickstart.md) - Integration guide

---

*Generated by /speckit.plan | 2026-02-03*
