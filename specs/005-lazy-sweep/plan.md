# Implementation Plan: Lazy Sweep for Garbage Collection

**Branch**: `005-lazy-sweep` | **Date**: 2026-01-31 | **Spec**: [link](./spec.md)
**Input**: Feature specification from `/specs/005-lazy-sweep/spec.md`

## Summary

Replace synchronous full-heap sweep during garbage collection with incremental lazy sweep performed during allocation operations. This eliminates stop-the-world (STW) pause times by spreading sweep work over normal allocation operations, reducing pause complexity from O(pages + objects) to O(1) amortized per allocation.

## Technical Context

**Language/Version**: Rust 1.75+ (standard library only, no external crates)  
**Primary Dependencies**: std::sync::atomic (for concurrent marking), std::thread (for parallelism), std::sync::Barrier, std::sync::Mutex  
**Storage**: N/A (in-memory garbage collector, heap managed internally)  
**Testing**: cargo test, ./test.sh, ./clippy.sh, ./miri-test.sh (from AGENTS.md)  
**Target Platform**: Cross-platform (Linux, macOS, Windows - x86_64 and aarch64)  
**Project Type**: Rust library crate (rudo-gc)  
**Performance Goals**: O(1) amortized allocation latency, eliminate STW pause times during sweep, bound maximum pause to under 100 microseconds per allocation-triggered sweep  
**Constraints**: Memory safety (NON-NEGOTIABLE per constitution), API consistency, performance-first design, cross-platform reliability  
**Scale/Scope**: GC library for Rust applications targeting high-performance use cases (gaming, real-time systems, interactive applications)

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Gate | Status | Notes |
|------|--------|-------|
| I. Memory Safety (NON-NEGOTIABLE) | ✅ PASS | Unsafe code requires SAFETY comments; Miri tests required; lazy sweep doesn't introduce new unsafe patterns |
| II. Testing Discipline (NON-NEGOTIABLE) | ✅ PASS | Tests required; --test-threads=1 for GC interference; Miri for unsafe; all_dead optimization reduces unsafe surface |
| III. Performance-First Design | ✅ PASS | O(1) allocation maintained; STW pause elimination is core goal; batch size bounds per-allocation overhead |
| IV. API Consistency | ✅ PASS | snake_case functions, PascalCase types, SCREAMING_SNAKE_CASE constants; follows existing patterns |
| V. Cross-Platform Reliability | ✅ PASS | Consistent behavior across x86_64/aarch64, Linux/macOS/Windows; no platform-specific code |

**Post-Phase 1 Re-evaluation**: All gates continue to pass. Design decisions align with constitution principles.

## Project Structure

### Documentation (this feature)

```text
specs/005-lazy-sweep/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (research findings)
├── data-model.md        # Phase 1 output (entity definitions)
├── quickstart.md        # Phase 1 output (implementation guide)
├── contracts/           # Phase 1 output (API specifications)
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created by /speckit.plan)
```

### Source Code (repository root)

```text
crates/rudo-gc/
├── src/
│   ├── lib.rs           # Public API exports
│   ├── gc/
│   │   └── gc.rs        # GC core logic (lazy sweep functions)
│   └── heap.rs          # Heap management (page flags, allocation path)
├── Cargo.toml           # Feature flag configuration
└── tests/
    ├── lazy_sweep.rs    # New integration tests
    └── benchmarks/
        └── sweep_comparison.rs  # Performance benchmarks
```

**Structure Decision**: The feature modifies existing library structure within the rudo-gc crate, adding new tests and benchmarks while maintaining the existing code organization.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| N/A | No constitution violations requiring justification | N/A |

---

## Phase 0: Research & Clarifications

### Unknowns to Resolve

The following technical decisions need research before implementation:

1. **Safepoint trigger frequency**: How often should lazy sweep work occur during check_safepoint() (currently ~0.5% per allocation)?
2. **All-dead optimization trigger**: What threshold of dead objects should trigger the "all-dead" flag?
3. **Page scanning strategy**: What is the best approach to avoid O(N) scan when looking for pages needing sweep?

### Research Tasks

1. Research "should_do_lazy_sweep" probability and its impact on heap growth vs. allocation overhead
2. Research best practices for dead object counting thresholds in garbage collectors
3. Research efficient page lookup structures (cursors, separate lists, bitmaps)

---

## Phase 1: Design & Contracts

### Key Entities (from spec)

1. **PageHeader**: Per-page metadata with sweep flags and dead object counter
2. **Sweep Flags**: PAGE_FLAG_NEEDS_SWEEP, PAGE_FLAG_ALL_DEAD bit flags
3. **Dead Object Counter**: u16 count per page for "all-dead" optimization
4. **Free List**: Per-page linked list of reclaimed objects
5. **~~Lazy Sweep Batch~~**: ~~Fixed batch of 16 objects per sweep operation~~ **(REMOVED: Batch limit removed due to bugs in breakpoint recovery)**

### API Contracts

**Public API Functions** (from spec):

```text
sweep_pending(num_pages: usize) -> usize
    Sweeps up to num_pages pages that need sweeping.
    Returns: Number of pages actually swept.

pending_sweep_pages() -> usize
    Returns: Count of pages awaiting sweep.
```

---

## Completed Artifacts

**Phase 0 - Research**:
- [x] `research.md` - Technical decisions with rationale

**Phase 1 - Design & Contracts**:
- [x] `data-model.md` - Entity definitions and state transitions
- [x] `contracts/api.md` - Public API specifications
- [x] `quickstart.md` - Implementation guide with 4-day schedule
- [x] Agent context updated (AGENTS.md)

## Summary of Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Safepoint frequency | Adaptive (threshold + probability) | Balances heap growth prevention with allocation overhead |
| All-dead trigger | dead_count == allocated_count | Simple counter, correctly identifies entirely-dead pages |
| Page scanning | O(N) scan for MVP, lists for optimization | Simple to implement, optimize later based on profiling |

## Next Steps

1. ✅ Phase 0: Complete (research.md created)
2. ✅ Phase 1: Complete (data-model.md, contracts/, quickstart.md, agent context)
3. **Phase 2**: Generate tasks.md using `/speckit.tasks` command
