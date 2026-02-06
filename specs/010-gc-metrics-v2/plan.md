# Implementation Plan: Extended GC Metrics System

**Branch**: `010-gc-metrics-v2` | **Date**: 2026-02-06 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/010-gc-metrics-v2/spec.md`

## Summary

Extend rudo-gc's minimal `GcMetrics` system to provide phase-level timing breakdown (clear/mark/sweep), surface existing `MarkStats` incremental data through the public API, add process-level cumulative statistics via `GlobalMetrics`, expose real-time heap size queries, and maintain a GC history ring buffer for trend analysis. All changes are additive—existing API unchanged—and thread-safe via atomics and the GC handshake serialization guarantee.

> **📚 Implementation Reference**: This plan provides high-level architecture and design decisions. For detailed code examples, complete test implementations, and step-by-step integration patterns, see `docs/metrics-improvement-plan-v2.md` (especially Sections 4, 6, and 8).

## Technical Context

**Language/Version**: Rust 1.75+ (stable)
**Primary Dependencies**: None (std-only: `std::sync::atomic`, `std::cell::UnsafeCell`, `std::time`)
**Storage**: In-memory only (thread-local `Cell`, static atomics, static ring buffer)
**Testing**: `cargo test` with `--test-threads=1` (GC interference avoidance), Miri for `UnsafeCell` safety
**Target Platform**: Cross-platform (Linux, macOS, Windows; x86_64, aarch64)
**Project Type**: Single Rust workspace crate (`rudo-gc`)
**Performance Goals**: <1% overhead on GC pause times; heap queries <1μs
**Constraints**: No new external dependencies; no locking on read paths; `Relaxed` atomics for informational counters
**Scale/Scope**: ~250 LOC added to `metrics.rs`; ~50 LOC instrumentation across 4 collection functions in `gc/gc.rs`; ~10 LOC re-exports in `lib.rs`

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Memory Safety (NON-NEGOTIABLE) | PASS | `GcHistory` uses `UnsafeCell` with single-writer (GC handshake) + atomic write-index. Requires `// SAFETY:` comments. Miri tests required. All other new code is safe Rust. |
| II. Testing Discipline (NON-NEGOTIABLE) | PASS | Unit tests for each new struct, integration tests for phase timing and multi-threaded counters, Miri tests for `GcHistory` `UnsafeCell`. `--test-threads=1` for all GC tests. |
| III. Performance-First Design | PASS | Phase timing: ~60ns overhead (3× `Instant::now()`). Global counters: `Relaxed` atomics (~1 cycle each). History push: single array write + atomic increment. All negligible vs GC pause times (μs–ms). |
| IV. API Consistency | PASS | Follows Rust conventions: `snake_case` functions, `PascalCase` types. Consistent with existing `last_gc_metrics()` pattern. Doc comments with examples on all public items. `#[must_use]` on query functions. |
| V. Cross-Platform Reliability | PASS | Standard library atomics only. No platform-specific code. `Duration`, `Instant`, `AtomicUsize`, `AtomicU64` are portable. |

**Gate result**: ALL PASS — proceed to Phase 0.

### Post-Design Re-Check (after Phase 1)

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Memory Safety | PASS | `GcHistory` `UnsafeCell` has documented SAFETY invariants (single-writer via GC handshake + atomic publish). `PhaseTimer` and `GlobalMetrics` are safe Rust. `CollectResult` is internal, no unsafe. Heap queries use existing `unsafe` access pattern (`&*h.tcb.heap.get()`) — already audited. |
| II. Testing Discipline | PASS | Test plan: unit tests per struct, integration tests for each phase, Miri test for `GcHistory`. All use `--test-threads=1`. |
| III. Performance-First | PASS | Phase timing: 3–6× `Instant::now()` = 60–150ns. `GlobalMetrics`: `Relaxed` atomics = ~8 cycles. `GcHistory::push()`: 1 array write + 1 atomic. Total overhead: <250ns per GC, <0.01% of typical pause. |
| IV. API Consistency | PASS | `snake_case` functions (`global_metrics`, `gc_history`, `current_heap_size`). `PascalCase` types (`GlobalMetrics`, `GcHistory`). `#[must_use]` on all query functions. Doc comments with examples on all public items. |
| V. Cross-Platform | PASS | No platform-specific code. `Instant`, `Duration`, `AtomicUsize`, `AtomicU64`, `UnsafeCell` are all portable. |

**Post-design gate result**: ALL PASS — design is constitution-compliant.

## Project Structure

### Documentation (this feature)

```text
specs/010-gc-metrics-v2/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   └── api.md           # Public API contract
└── tasks.md             # Phase 2 output (created by /speckit.tasks)
```

### Source Code (repository root)

```text
crates/rudo-gc/src/
├── metrics.rs           # PRIMARY: Extended GcMetrics, GlobalMetrics, GcHistory, heap queries
├── gc/
│   └── gc.rs            # MODIFIED: PhaseTimer instrumentation in 4 collection functions
├── gc/
│   └── incremental.rs   # UNCHANGED: MarkStats/FallbackReason already exist, read-only access
└── lib.rs               # MODIFIED: Additional re-exports

crates/rudo-gc/tests/
└── metrics_tests.rs     # NEW: Integration tests for extended metrics
```

**Structure Decision**: Single crate modification. All new code lives in `metrics.rs` (the metrics module). Collection functions in `gc/gc.rs` receive minimal instrumentation changes (add `PhaseTimer` + populate new `GcMetrics` fields). No new crates, no new modules.

## Complexity Tracking

No constitution violations. No complexity justification needed.

## Implementation Phases

### Phase 1: Extend `GcMetrics` + `PhaseTimer` + Incremental Stats (P0)

**Files**: `metrics.rs`, `gc/gc.rs`

| Component | Description |
|-----------|-------------|
| `GcMetrics` new fields | `clear_duration`, `mark_duration`, `sweep_duration`, `objects_marked`, `dirty_pages_scanned`, `slices_executed`, `fallback_occurred`, `fallback_reason` |
| `PhaseTimer` (internal) | Helper struct with `start()`, `end_clear()`, `end_mark()`, `end_sweep()` methods |
| `FallbackReason` re-export | Re-export from `gc::incremental` through `metrics` module |
| Collection function instrumentation | Thread `PhaseTimer` through: `perform_multi_threaded_collect()`, `perform_multi_threaded_collect_full()`, `perform_single_threaded_collect_with_wake()`, `perform_single_threaded_collect_full()` |
| `MarkStats` integration | Read `IncrementalMarkState::global().stats()` atomics when populating `GcMetrics` in collection functions |

**Key insight**: `collect_major()` dispatches to `collect_major_incremental()` or `collect_major_stw()` internally. Phase timing wraps the caller level (the `perform_*` functions), not the inner functions. The inner functions are called within the heap closure for single-threaded paths.

**Challenge**: `perform_single_threaded_collect_with_wake()` calls `collect_major(heap)` inside `with_heap()`. Phase timing must happen inside the closure since `collect_major()` handles clear/mark/sweep internally. For this path, `mark_duration` can be sourced from `MarkStats::mark_time_ns` for incremental collections, and the overall `duration` minus `sweep_duration` serves as an approximation for STW.

### Phase 2: `GlobalMetrics` + Heap Queries (P1)

**Files**: `metrics.rs`, `lib.rs`

| Component | Description |
|-----------|-------------|
| `GlobalMetrics` struct | 8 atomic counters (collections, bytes, objects, pause time, per-type counts, fallbacks) |
| Static singleton | `static GLOBAL_METRICS: GlobalMetrics = GlobalMetrics::new()` |
| `global_metrics()` accessor | Returns `&'static GlobalMetrics` |
| `record_metrics()` update | Increment global counters after updating thread-local (existing choke-point) |
| `current_heap_size()` | Read `HEAP.try_with(|h| ... .total_allocated())` — returns 0 if no heap |
| `current_young_size()` | Read `HEAP.try_with(|h| ... .young_allocated())` |
| `current_old_size()` | Read `HEAP.try_with(|h| ... .old_allocated())` |

**Key insight**: `record_metrics()` is the single aggregation point — all 4 collection functions converge here. No additional call sites needed. `Relaxed` ordering sufficient since counters are informational.

**Key insight**: Heap queries use `try_with()` to handle threads without a heap (returns 0). Uses existing `LocalHeap` methods: `total_allocated()`, `young_allocated()`, `old_allocated()`.

### Phase 3: GC History Ring Buffer (P2)

**Files**: `metrics.rs`, `lib.rs`

| Component | Description |
|-----------|-------------|
| `GcHistory` struct | `UnsafeCell<[GcMetrics; 64]>` + `AtomicUsize` write index |
| `unsafe impl Sync` | Single-writer (GC handshake), atomic write-index for reader safety |
| `push()` (internal) | Write to slot, then `fetch_add(1, Release)` on write index |
| `total_recorded()` | `write_idx.load(Acquire)` — may exceed buffer size |
| `recent(n)` | Read last N entries (newest first), capped to buffer size |
| `average_pause_time(n)` | Compute average from `recent(n)` durations |
| `max_pause_time(n)` | Compute max from `recent(n)` durations |
| Static singleton | `static GC_HISTORY: GcHistory = GcHistory::new()` |
| `gc_history()` accessor | Returns `&'static GcHistory` |
| `record_metrics()` update | Call `GC_HISTORY.push(metrics)` after global counter updates |

**Safety argument**: Writes are serialized by the GC handshake — only one collection runs at a time. The write-index uses `Release` ordering after slot write, and readers use `Acquire` on `total_recorded()`, establishing happens-before. A reader may see a partially-written slot only if it races with a concurrent write, but since `GcMetrics` is `Copy` and all fields are primitive, a torn read produces a valid (if slightly incorrect) `GcMetrics`.

## Key Design Decisions

1. **Additive-only changes to `GcMetrics`**: New fields default to zero. Existing callers unaffected. `#[non_exhaustive]` deferred to a future semver-minor.

2. **`PhaseTimer` is internal**: Not exposed in public API. Exists solely to avoid duplicating `Instant::now()` calls across collection functions.

3. **`Relaxed` ordering for `GlobalMetrics`**: Counters are informational. They don't synchronize any other state. `Relaxed` is correct and fastest.

4. **`try_with()` for heap queries**: Handles the edge case where a thread hasn't initialized its heap (e.g., during startup or in non-GC threads). Returns 0 instead of panicking.

5. **Ring buffer size = 64**: Provides enough history for trend analysis without excessive memory. Power-of-2 enables efficient modulo via bitmask.

6. **No `#[non_exhaustive]` yet**: Adding it would break downstream code using struct literals. Defer to a separate semver-minor change. Document intent.

7. **Heap queries are per-thread**: `current_heap_size()` returns this thread's heap. Cross-thread aggregation requires locking the thread registry — deferred to avoid contention on read paths.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Phase timing overhead | Low | Low | `Instant::now()` is ~20ns; negligible vs μs–ms pauses |
| Atomic contention on `GlobalMetrics` | Very Low | Low | `Relaxed` ordering; written once per GC cycle |
| `GcHistory` `UnsafeCell` unsoundness | Medium | High | Single-writer invariant from GC handshake; Miri testing; SAFETY audit |
| `GcMetrics` struct size bloat | Low | Low | +56 bytes; stack-allocated, copied infrequently |
| `IncrementalMajor` never set in metrics | Known Bug | Medium | Currently `collect_major()` sets `Major` even for incremental. Fix by checking `IncrementalMarkState::global().config().enabled` |
