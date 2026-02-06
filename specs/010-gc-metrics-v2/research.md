# Research: Extended GC Metrics System

**Branch**: `010-gc-metrics-v2` | **Date**: 2026-02-06

## Research Questions

### R1: How to instrument phase timing in single-threaded collection paths?

**Context**: `perform_single_threaded_collect_with_wake()` and `perform_single_threaded_collect_full()` delegate to `collect_major(heap)` which internally calls `collect_major_stw()` or `collect_major_incremental()`. These inner functions handle clear/mark/sweep phases internally, so a `PhaseTimer` can't be threaded at the caller level.

**Decision**: Instrument the inner functions (`collect_major_stw()`, `collect_major_incremental()`) to return phase timings alongside the reclaimed count. This avoids duplicating phase boundaries and keeps timing accurate.

**Approach**: Change `collect_major_stw()` and `collect_major_incremental()` to return a `CollectResult` struct:

```rust
struct CollectResult {
    objects_reclaimed: usize,
    timer: PhaseTimer,
}
```

Then `collect_major()` returns `CollectResult`, and the `perform_*` functions extract timing from it.

**Rationale**: This keeps phase boundaries precise (timing wraps the actual clear/mark/sweep calls), avoids approximations, and doesn't require restructuring the collection logic.

**Alternatives considered**:
- *Wrap at perform_* level*: Can't — `collect_major()` is called inside `with_heap()` closure, and inner phases aren't visible.
- *Read `MarkStats::mark_time_ns` for mark duration*: Only available for incremental collections; doesn't cover STW mark phase.
- *Time the `collect_major()` call and subtract sweep time*: Imprecise; can't separate clear and mark.

### R2: How to correctly set `CollectionType::IncrementalMajor` in metrics?

**Context**: Currently, all major collections are reported as `CollectionType::Major` even when incremental marking is enabled. The `collect_major()` function checks `IncrementalConfig::enabled` to dispatch, but the caller doesn't know which path was taken.

**Decision**: Include the `CollectionType` in the `CollectResult` struct. `collect_major_incremental()` sets `IncrementalMajor`, `collect_major_stw()` sets `Major`. This propagates to `record_metrics()`.

**Rationale**: The type should reflect what actually happened, not what was requested. The inner function knows the truth.

**Alternatives considered**:
- *Check `IncrementalConfig::enabled` at the caller*: Doesn't account for fallback (incremental → STW).
- *Check `MarkStats::fallback_occurred` after the fact*: Incomplete — doesn't distinguish "never incremental" from "incremental that fell back."

### R3: `UnsafeCell` soundness for `GcHistory` ring buffer

**Context**: `GcHistory` uses `UnsafeCell<[GcMetrics; 64]>` for a lock-free ring buffer. Need to validate the safety argument.

**Decision**: The design is sound under these invariants:
1. **Single writer**: Only `record_metrics()` calls `push()`. `record_metrics()` is called from the collector thread after the GC handshake (mutator threads are paused). No concurrent writes.
2. **Atomic publish**: `write_idx.fetch_add(1, Release)` after slot write. Readers use `write_idx.load(Acquire)`. This creates a happens-before relationship: if a reader sees index N, all writes to slot N-1 are visible.
3. **No torn reads on active slot**: A reader calling `recent()` might read a slot being overwritten (if >64 collections have occurred and the buffer wraps). Since `GcMetrics` is `Copy` with only primitive fields (`Duration` = two `u64`s, `usize`, `bool`, `u8`), a torn read would still produce a valid `GcMetrics` struct (just with mixed old/new values). This is acceptable for informational data.

**Miri validation**: Required. Miri tests must cover:
- Concurrent read during write (spawn reader thread, write from main)
- Ring buffer wrap-around (>64 entries)
- `recent()` after various fill levels (0, 1, 63, 64, 128)

**Rationale**: This pattern is well-established (SPSC ring buffer with atomic index). The single-writer guarantee from the GC handshake makes it simpler than general SPSC queues.

**Alternatives considered**:
- *`Mutex<Vec<GcMetrics>>`*: Adds lock contention on every GC cycle and every read. Unnecessary given single-writer guarantee.
- *`crossbeam::ArrayQueue`*: External dependency; project avoids external deps for core functionality.
- *Thread-local history*: Doesn't aggregate across threads; less useful for global trends.

### R4: `Instant::now()` overhead and cross-platform behavior

**Context**: Phase timing requires 3–6 `Instant::now()` calls per GC cycle. Need to confirm overhead is negligible.

**Decision**: Overhead is negligible.
- **Linux**: `clock_gettime(CLOCK_MONOTONIC)` via VDSO — ~20ns per call
- **macOS**: `mach_absolute_time()` — ~15ns per call
- **Windows**: `QueryPerformanceCounter()` — ~25ns per call
- **Total per GC cycle**: ~60–150ns for 3–6 calls
- **GC pause times**: Typically 10μs–10ms

Overhead is <0.01% of the shortest realistic GC pause.

**Rationale**: Rust `std::time::Instant` is the standard cross-platform monotonic timer. No platform-specific code needed.

### R5: Atomic ordering for `GlobalMetrics` counters

**Context**: Need to decide atomic ordering for cumulative counters.

**Decision**: `Relaxed` ordering for all `GlobalMetrics` operations.

**Rationale**:
1. Counters are informational — they don't synchronize other state.
2. No happens-before relationships needed between counter increments and reads.
3. `Relaxed` is a single cycle on x86 (same as non-atomic) and very cheap on ARM.
4. Multiple threads may write concurrently (if multi-threaded collection triggers on different threads), but `fetch_add` with `Relaxed` is still atomic — the total will be correct, just not ordered w.r.t. other memory operations.
5. Readers may see slightly stale values, which is acceptable for monitoring data.

**Alternatives considered**:
- *`SeqCst`*: Provides total ordering but unnecessary overhead. No consumer needs to correlate counter reads with other memory operations.
- *`Acquire/Release`*: Provides happens-before between writer and reader, but unnecessary since readers don't depend on any state guarded by these counters.

### R6: gc-arena metrics comparison — what to adopt, what to skip

**Context**: The v2 plan references gc-arena's metrics system. User provided access to `learn-projects/gc-arena` for reference.

**Decision**: Adopt the **spirit** (cumulative stats, per-cycle breakdown, real-time queries) but not the mechanism:

| gc-arena Concept | rudo-gc Adaptation | Reason |
|-----------------|-------------------|--------|
| `Metrics::allocation_debt()` | Not adopted | rudo-gc uses threshold-based triggers, not debt-based pacing |
| `Pacing` struct | Not adopted | rudo-gc has `IncrementalConfig` for pacing |
| `Rc<MetricsInner>` with `Cell` counters | `GlobalMetrics` with `AtomicUsize` | Multi-threaded; can't use `Rc`/`Cell` |
| `mark_external_allocation()` | Not adopted | rudo-gc's global GC doesn't track external allocations |
| Per-cycle stats (`allocated_gc_bytes`, `marked_gcs`) | Per-collection snapshot via `GcMetrics` | Extended with phase timing + incremental stats |
| Cumulative counters | `GlobalMetrics` static singleton | Same concept, atomic instead of `Cell` |

**Rationale**: gc-arena is single-threaded, arena-scoped, and debt-driven. rudo-gc is multi-threaded, global-GC, and threshold-based. The data we expose is similar (cumulative stats + per-cycle breakdown), but the storage and access patterns are fundamentally different.

## Summary of Decisions

| # | Question | Decision |
|---|----------|----------|
| R1 | Phase timing in single-threaded paths | Return `CollectResult` from inner functions |
| R2 | `CollectionType::IncrementalMajor` | Set in `collect_major_incremental()`, propagate via `CollectResult` |
| R3 | `GcHistory` UnsafeCell soundness | Sound under single-writer + atomic publish; Miri tests required |
| R4 | `Instant::now()` overhead | ~60-150ns total per GC; negligible (<0.01% of pause) |
| R5 | Atomic ordering | `Relaxed` for all `GlobalMetrics` ops |
| R6 | gc-arena comparison | Adopt spirit (cumulative + per-cycle), skip mechanism (debt, pacing, Rc) |

All unknowns resolved. No NEEDS CLARIFICATION items remain.
