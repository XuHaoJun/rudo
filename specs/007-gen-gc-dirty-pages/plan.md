# Implementation Plan: Generational GC Dirty Page Tracking

**Branch**: `007-gen-gc-dirty-pages` | **Date**: 2026-02-03 | **Spec**: [spec.md](./spec.md)  
**Input**: Feature specification from `/specs/007-gen-gc-dirty-pages/spec.md`

---

## Summary

Optimize minor GC pause times by implementing dirty page tracking. The current implementation iterates O(num_pages) to find dirty old-generation objects. This plan introduces a mutex-protected dirty page list that reduces complexity to O(dirty_pages), targeting 2-5x reduction in minor GC pause times.

**Key Insight**: The current per-object dirty bitmap is efficient for marking individual objects, but the page-level iteration to find those objects is the bottleneck. By maintaining a list of pages that contain dirty objects, we skip scanning pages that are known to be clean.

---

## Technical Context

**Language/Version**: Rust 1.75+ (stable)  
**Primary Dependencies**: parking_lot (mutex), std::sync::atomic  
**Storage**: N/A (in-memory GC)  
**Testing**: cargo test, Miri, loom  
**Target Platform**: Linux, macOS, Windows (x86_64, aarch64)  
**Project Type**: Single crate library  
**Performance Goals**: 2-5x reduction in minor GC pause times  
**Constraints**: <5% write barrier overhead increase, <0.1% memory overhead  
**Scale/Scope**: Heaps with 1000s of pages, 10-100 dirty pages per GC cycle

---

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-checked after Phase 1 design.*

| Principle | Status | Evidence |
|-----------|--------|----------|
| I. Memory Safety | PASS | All unsafe code has SAFETY comments; Miri tests required |
| II. Testing Discipline | PASS | Unit tests + integration tests + loom + Miri planned |
| III. Performance-First | PASS | O(dirty_pages) complexity; benchmarks required |
| IV. API Consistency | PASS | Internal API follows naming conventions |
| V. Cross-Platform | PASS | Uses std atomics; no platform-specific code |

**Post-Design Re-check**: PASS - Design uses proven patterns (Chez Scheme mutex-protected list), memory ordering is explicit, testing strategy covers concurrency.

---

## Project Structure

### Documentation (this feature)

```text
specs/007-gen-gc-dirty-pages/
├── plan.md              # This file
├── spec.md              # Feature specification
├── research.md          # Phase 0: rudo-gc + Chez Scheme analysis
├── data-model.md        # Phase 1: Entity definitions
├── quickstart.md        # Phase 1: Implementation guide
├── contracts/
│   └── api.md           # Phase 1: Internal API contracts
└── checklists/
    └── requirements.md  # Specification quality checklist
```

### Source Code (repository root)

```text
crates/rudo-gc/
├── src/
│   ├── heap.rs          # MODIFY: LocalHeap, PageHeader
│   ├── cell.rs          # MODIFY: write_barrier
│   ├── gc/
│   │   └── gc.rs        # MODIFY: mark_minor_roots*
│   └── lib.rs
├── tests/
│   ├── dirty_page_list.rs       # NEW: Unit tests
│   ├── minor_gc_optimized.rs    # NEW: Integration tests
│   └── loom_dirty_page_list.rs  # NEW: Loom tests
└── Cargo.toml           # MODIFY: Add parking_lot dependency
```

**Structure Decision**: Single crate modification. All changes are internal to rudo-gc; no new crates needed.

---

## Research Summary

See [research.md](./research.md) for detailed analysis.

### Key Findings

1. **Current Bottleneck**: `mark_minor_roots` iterates `heap.all_pages()` which is O(num_pages), even though only a small subset have dirty objects.

2. **Chez Scheme Pattern**: Uses mutex-protected dirty segment lists with a flag (`min_dirty_byte != 0xff`) to prevent duplicates. Proven in production.

3. **Available Flag Bit**: `PageHeader.flags` is `AtomicU8` with bits 0-3 used. Bit 4 (0x10) is available for `PAGE_FLAG_DIRTY_LISTED`.

4. **Write Barrier Integration**: Current barrier sets dirty bit but doesn't track pages. Adding `heap.add_to_dirty_pages(header)` after `set_dirty()` is straightforward.

5. **Synchronization**: parking_lot::Mutex is fast (2 cycles uncontended). Double-check pattern avoids lock for already-listed pages.

### Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Data structure | Vec + Mutex | Simple, fast, proven (Chez) |
| Duplicate prevention | Atomic flag | O(1) check, no Vec scan |
| GC scanning | Snapshot pattern | Lock-free scanning |
| Memory ordering | Acquire/Release | Correct synchronization |

---

## Implementation Phases

### Phase 1: Foundation (heap.rs)

**Files**: `crates/rudo-gc/src/heap.rs`, `Cargo.toml`

**Tasks**:
1. Add parking_lot dependency to Cargo.toml
2. Add `PAGE_FLAG_DIRTY_LISTED = 0x10` constant
3. Add `is_dirty_listed()`, `set_dirty_listed()`, `clear_dirty_listed()` to PageHeader
4. Add `dirty_pages`, `dirty_pages_snapshot`, statistics fields to LocalHeap
5. Add `add_to_dirty_pages()`, `take_dirty_pages_snapshot()`, `dirty_pages_iter()`, `clear_dirty_pages_snapshot()` methods
6. Update `LocalHeap::new()` to initialize new fields

**Tests**: Unit tests for add/snapshot/clear operations

### Phase 2: Write Barrier (cell.rs)

**Files**: `crates/rudo-gc/src/cell.rs`

**Tasks**:
1. Update `write_barrier()` for small objects to call `add_to_dirty_pages()`
2. Update `write_barrier()` for large objects to call `add_to_dirty_pages()`
3. Ensure correct NonNull construction from raw pointers

**Tests**: Verify dirty page list is populated on old-gen mutations

### Phase 3: Minor Collection (gc.rs)

**Files**: `crates/rudo-gc/src/gc/gc.rs`

**Tasks**:
1. Update `mark_minor_roots()` to use dirty page snapshot pattern
2. Update `mark_minor_roots_multi()` with same pattern
3. Update `mark_minor_roots_parallel()` with same pattern
4. Clear dirty bits and flags after scanning each page
5. Clear snapshot at end of GC

**Tests**: Integration tests for minor GC correctness

### Phase 4: Testing

**Files**: `crates/rudo-gc/tests/`

**Tasks**:
1. Create `dirty_page_list.rs` with unit tests
2. Create `minor_gc_optimized.rs` with integration tests
3. Create `loom_dirty_page_list.rs` with concurrency tests
4. Run full test suite: `./test.sh`
5. Run Miri: `./miri-test.sh`

### Phase 5: Benchmarking (Optional)

**Files**: `crates/rudo-gc/benches/`

**Tasks**:
1. Create benchmark comparing old vs new minor GC
2. Measure write barrier overhead
3. Document results

---

## Complexity Tracking

No violations requiring justification. Design is minimal:
- 1 new dependency (parking_lot)
- 4 new fields in LocalHeap
- 4 new methods in LocalHeap
- 3 new methods in PageHeader
- ~50 lines changed in write barrier
- ~100 lines changed in mark_minor_roots

---

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Mutex contention | Double-check pattern; parking_lot is fast |
| Race conditions | Explicit ordering; loom tests |
| Memory leaks | Clear list at end of each GC cycle |
| Test failures | Run full suite before/after each phase |

---

## Artifacts Generated

| Artifact | Status | Location |
|----------|--------|----------|
| research.md | Complete | `specs/007-gen-gc-dirty-pages/research.md` |
| data-model.md | Complete | `specs/007-gen-gc-dirty-pages/data-model.md` |
| contracts/api.md | Complete | `specs/007-gen-gc-dirty-pages/contracts/api.md` |
| quickstart.md | Complete | `specs/007-gen-gc-dirty-pages/quickstart.md` |
| tasks.md | Pending | Run `/speckit.tasks` to generate |

---

## Next Steps

1. Run `/speckit.tasks` to break down into actionable tasks
2. Implement Phase 1 (foundation)
3. Verify with `./test.sh` and `./clippy.sh`
4. Continue through phases
5. Run benchmarks to verify performance improvement
