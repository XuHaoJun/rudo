# Quickstart: Extended GC Metrics System

**Branch**: `010-gc-metrics-v2` | **Date**: 2026-02-06

## Overview

This guide walks through implementing the extended GC metrics system in order. Each phase can be implemented and tested independently.

> **💡 Implementation Reference**: For detailed code examples, complete test implementations, and integration patterns, refer to `docs/metrics-improvement-plan-v2.md`:
> - **Section 4.1**: Complete `PhaseTimer` instrumentation examples for all collection functions
> - **Section 8**: Full test code (unit and integration tests) ready to adapt
> - **Section 6**: Complete API usage examples
> - **Section 2**: Architecture constraints and design decisions

---

## Phase 1: Extend `GcMetrics` + Phase Timing (P0)

### Step 1a: Add new fields to `GcMetrics`

**File**: `crates/rudo-gc/src/metrics.rs`

Add 8 new fields to `GcMetrics` after the existing fields:

```rust
pub struct GcMetrics {
    // ... existing 7 fields unchanged ...

    // NEW: phase timing
    pub clear_duration: Duration,
    pub mark_duration: Duration,
    pub sweep_duration: Duration,

    // NEW: incremental marking stats
    pub objects_marked: usize,
    pub dirty_pages_scanned: usize,
    pub slices_executed: usize,
    pub fallback_occurred: bool,
    pub fallback_reason: FallbackReason,
}
```

Update `GcMetrics::new()` to initialize new fields to zero/false/`FallbackReason::None`.

Re-export `FallbackReason` from `gc::incremental`:

```rust
pub use crate::gc::incremental::FallbackReason;
```

### Step 1b: Add `PhaseTimer` helper

**File**: `crates/rudo-gc/src/metrics.rs` (or `gc/gc.rs` if preferred — it's internal)

```rust
pub(crate) struct PhaseTimer {
    pub clear: Duration,
    pub mark: Duration,
    pub sweep: Duration,
    current_start: Option<Instant>,
}

impl PhaseTimer {
    pub fn new() -> Self { /* all zero, current_start = None */ }
    pub fn start(&mut self) { self.current_start = Some(Instant::now()); }
    pub fn end_clear(&mut self) { self.clear = self.current_start.take().unwrap().elapsed(); }
    pub fn end_mark(&mut self) { self.mark = self.current_start.take().unwrap().elapsed(); }
    pub fn end_sweep(&mut self) { self.sweep = self.current_start.take().unwrap().elapsed(); }
}
```

### Step 1c: Add `CollectResult` return type

**File**: `crates/rudo-gc/src/gc/gc.rs`

```rust
struct CollectResult {
    objects_reclaimed: usize,
    timer: PhaseTimer,
    collection_type: crate::metrics::CollectionType,
}
```

### Step 1d: Instrument `collect_major_stw()`

Change return type from `usize` to `CollectResult`. Add `PhaseTimer`:

```rust
fn collect_major_stw(heap: &mut LocalHeap) -> CollectResult {
    let mut timer = PhaseTimer::new();

    timer.start();
    clear_all_marks_and_dirty(heap);
    timer.end_clear();

    timer.start();
    let objects_marked = mark_major_roots(heap);
    timer.end_mark();

    timer.start();
    let reclaimed = sweep_segment_pages(heap, false);
    let reclaimed_large = sweep_large_objects(heap, false);
    promote_all_pages(heap);
    timer.end_sweep();

    CollectResult {
        objects_reclaimed: reclaimed + reclaimed_large,
        timer,
        collection_type: crate::metrics::CollectionType::Major,
    }
}
```

### Step 1e: Instrument `collect_major_incremental()`

Same pattern, but set `collection_type` to `IncrementalMajor` and read `MarkStats`:

```rust
fn collect_major_incremental(heap: &mut LocalHeap) -> CollectResult {
    let mut timer = PhaseTimer::new();

    // Clear + snapshot phase
    timer.start();
    let heaps: [&LocalHeap; 1] = [&*heap];
    execute_snapshot(&heaps);
    timer.end_clear();

    // Mark phase (incremental slices)
    timer.start();
    loop {
        let result = mark_slice(heap, per_worker_budget);
        match result {
            MarkSliceResult::Complete { .. } => break,
            MarkSliceResult::Pending { .. } => {}
            MarkSliceResult::Fallback { reason } => { /* handle fallback */ break; }
        }
    }
    // ... final mark if needed ...
    timer.end_mark();

    // Sweep phase
    timer.start();
    let reclaimed = sweep_segment_pages(heap, false);
    let reclaimed_large = sweep_large_objects(heap, false);
    promote_all_pages(heap);
    timer.end_sweep();

    CollectResult {
        objects_reclaimed: reclaimed + reclaimed_large,
        timer,
        collection_type: crate::metrics::CollectionType::IncrementalMajor,
    }
}
```

### Step 1f: Update `collect_major()` to propagate `CollectResult`

```rust
fn collect_major(heap: &mut LocalHeap) -> CollectResult {
    let config = IncrementalMarkState::global().config();
    if config.enabled {
        collect_major_incremental(heap)
    } else {
        collect_major_stw(heap)
    }
}
```

### Step 1g: Instrument `perform_multi_threaded_collect()`

Add `PhaseTimer` around the existing phase boundaries (lines 335–450). Build extended `GcMetrics`:

```rust
let mark_stats = IncrementalMarkState::global().stats();
crate::metrics::record_metrics(crate::metrics::GcMetrics {
    duration,
    clear_duration: timer.clear,
    mark_duration: timer.mark,
    sweep_duration: timer.sweep,
    objects_marked: mark_stats.objects_marked.load(Relaxed),
    dirty_pages_scanned: mark_stats.dirty_pages_scanned.load(Relaxed),
    slices_executed: mark_stats.slices_executed.load(Relaxed),
    fallback_occurred: mark_stats.fallback_occurred.load(Relaxed),
    fallback_reason: FallbackReason::from_u32(mark_stats.fallback_reason.load(Relaxed)),
    // ... existing fields ...
});
```

### Step 1h: Instrument remaining `perform_*` functions

Apply the same pattern to:
- `perform_multi_threaded_collect_full()` — already has clear/mark/sweep phases
- `perform_single_threaded_collect_with_wake()` — use `CollectResult` from `collect_major()`
- `perform_single_threaded_collect_full()` — use `CollectResult` from `collect_major()`

For the single-threaded functions that call `collect_major()` inside `with_heap()`, the `CollectResult` propagates timing data:

```rust
crate::heap::with_heap(|heap| {
    if total_size > MAJOR_THRESHOLD {
        result = collect_major(heap);
    } else {
        // Minor: no clear phase, combined mark+sweep
        let mut timer = PhaseTimer::new();
        timer.start();
        let reclaimed = collect_minor(heap);
        timer.end_sweep();
        result = CollectResult {
            objects_reclaimed: reclaimed,
            timer,
            collection_type: CollectionType::Minor,
        };
    }
});
```

### Step 1i: Update `lib.rs` re-exports

```rust
pub use metrics::{last_gc_metrics, CollectionType, GcMetrics, FallbackReason};
```

### Step 1j: Tests

Unit tests in `metrics.rs`:
- `test_gc_metrics_new_fields_default_to_zero`
- `test_phase_timer_captures_durations`

Integration tests in `tests/metrics_tests.rs`:
- `test_phase_timing_sums_approximately`
- `test_incremental_metrics_populated`

---

## Phase 2: `GlobalMetrics` + Heap Queries (P1)

### Step 2a: Add `GlobalMetrics` struct

**File**: `crates/rudo-gc/src/metrics.rs`

```rust
pub struct GlobalMetrics {
    total_collections: AtomicUsize,
    total_minor_collections: AtomicUsize,
    total_major_collections: AtomicUsize,
    total_incremental_collections: AtomicUsize,
    total_bytes_reclaimed: AtomicUsize,
    total_objects_reclaimed: AtomicUsize,
    total_pause_ns: AtomicU64,
    total_fallbacks: AtomicUsize,
}

static GLOBAL_METRICS: GlobalMetrics = GlobalMetrics::new();

#[must_use]
pub fn global_metrics() -> &'static GlobalMetrics { &GLOBAL_METRICS }
```

### Step 2b: Add read accessors

8 methods, all `#[inline]`, all `#[must_use]`, all `Relaxed` ordering.

### Step 2c: Update `record_metrics()`

Add global counter increments after existing thread-local update:

```rust
pub fn record_metrics(metrics: GcMetrics) {
    // ... existing thread-local code ...

    // NEW: global counters
    let g = global_metrics();
    g.total_collections.fetch_add(1, Relaxed);
    g.total_bytes_reclaimed.fetch_add(metrics.bytes_reclaimed, Relaxed);
    g.total_objects_reclaimed.fetch_add(metrics.objects_reclaimed, Relaxed);
    g.total_pause_ns.fetch_add(metrics.duration.as_nanos() as u64, Relaxed);
    match metrics.collection_type {
        CollectionType::Minor => g.total_minor_collections.fetch_add(1, Relaxed),
        CollectionType::Major => g.total_major_collections.fetch_add(1, Relaxed),
        CollectionType::IncrementalMajor => {
            g.total_incremental_collections.fetch_add(1, Relaxed);
            if metrics.fallback_occurred {
                g.total_fallbacks.fetch_add(1, Relaxed);
            }
        }
        CollectionType::None => {}
    };
}
```

### Step 2d: Add heap query functions

```rust
#[must_use]
pub fn current_heap_size() -> usize {
    crate::heap::HEAP
        .try_with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated())
        .unwrap_or(0)
}
// Similarly: current_young_size(), current_old_size()
```

### Step 2e: Update `lib.rs` re-exports

```rust
pub use metrics::{
    last_gc_metrics, global_metrics,
    current_heap_size, current_young_size, current_old_size,
    CollectionType, GcMetrics, GlobalMetrics, FallbackReason,
};
```

### Step 2f: Tests

- `test_global_metrics_accumulate` — single-threaded
- `test_global_metrics_multi_threaded` — 4 threads
- `test_heap_queries_return_sane_values`
- `test_heap_queries_no_heap_returns_zero`

---

## Phase 3: GC History Ring Buffer (P2)

### Step 3a: Add `GcHistory` struct

**File**: `crates/rudo-gc/src/metrics.rs`

```rust
const HISTORY_SIZE: usize = 64;

pub struct GcHistory {
    buffer: UnsafeCell<[GcMetrics; HISTORY_SIZE]>,
    write_idx: AtomicUsize,
}

// SAFETY: Single writer (GC handshake). Atomic write-index for reader safety.
unsafe impl Sync for GcHistory {}

static GC_HISTORY: GcHistory = GcHistory::new();

#[must_use]
pub fn gc_history() -> &'static GcHistory { &GC_HISTORY }
```

### Step 3b: Implement methods

- `push()` (internal) — write slot, then `fetch_add(1, Release)`
- `total_recorded()` — `write_idx.load(Acquire)`
- `recent(n)` — read last N entries, newest first
- `average_pause_time(n)` — compute from `recent(n)`
- `max_pause_time(n)` — compute from `recent(n)`

### Step 3c: Update `record_metrics()`

Add at the end:

```rust
GC_HISTORY.push(metrics);
```

### Step 3d: Update `lib.rs` re-exports

Add `gc_history` and `GcHistory`.

### Step 3e: Tests

- `test_history_ring_buffer` — push and read
- `test_history_wrap_around` — push >64 entries, verify only last 64 retained
- `test_history_average_pause` — verify computation
- `test_history_empty` — `average_pause_time(10)` returns `Duration::ZERO`
- Miri test: concurrent read during write

---

## Verification Checklist

After each phase:

1. `cargo fmt --all`
2. `./clippy.sh` — zero warnings
3. `./test.sh` — all tests pass
4. For Phase 3: `./miri-test.sh` — UnsafeCell safety verified
5. Doc comments on all public items with examples
