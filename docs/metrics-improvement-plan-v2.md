# rudo-gc Metrics Improvement Plan v2

**Date**: 2026-02-06
**Status**: Draft

---

## 1. Current State

### 1.1 Existing `metrics.rs` (82 lines)

The current metrics system is minimal — a thread-local snapshot of the last GC run:

```rust
pub struct GcMetrics {
    pub duration: Duration,
    pub bytes_reclaimed: usize,
    pub bytes_surviving: usize,
    pub objects_reclaimed: usize,
    pub objects_surviving: usize,
    pub collection_type: CollectionType,  // None | Minor | Major | IncrementalMajor
    pub total_collections: usize,
}

// Storage: thread_local Cell<GcMetrics>
pub fn last_gc_metrics() -> GcMetrics;
pub fn record_metrics(metrics: GcMetrics);
```

### 1.2 Existing `MarkStats` (in `gc/incremental.rs`)

Incremental marking already tracks rich statistics via atomic counters, but this
data is **not surfaced** through the public API:

```rust
pub struct MarkStats {
    pub objects_marked: AtomicUsize,
    pub dirty_pages_scanned: AtomicUsize,
    pub slices_executed: AtomicUsize,
    pub mark_time_ns: AtomicU64,
    pub fallback_occurred: AtomicBool,
    pub fallback_reason: AtomicU32,  // FallbackReason enum
}
```

### 1.3 Existing `tracing` Feature

Feature-gated structured tracing spans exist in `src/tracing.rs` and
`src/gc/tracing.rs` (using the `tracing` crate). These emit `DEBUG`-level
spans/events for phase transitions, incremental slices, fallbacks, etc.

### 1.4 Key Gaps

| Gap | Impact | Priority |
|-----|--------|----------|
| `MarkStats` not unified with `GcMetrics` | Incremental marking stats invisible to users | P0 |
| No phase-level timing breakdown | Cannot identify slow phases | P0 |
| No cumulative / cross-thread statistics | No trend analysis, no global picture | P1 |
| No real-time heap queries | Must wait for GC to get any data | P1 |
| No GC history | Cannot compute averages / detect regressions | P2 |

---

## 2. Architecture Constraints

rudo-gc's architecture differs significantly from single-threaded arena-based
collectors (like gc-arena). The metrics design must respect these constraints:

1. **Multi-threaded with per-thread heaps**: Each thread owns a `LocalHeap`.
   GC can be single-threaded or multi-threaded (collector thread + rendezvous).
   Metrics storage must handle both cases.

2. **Thread-local `record_metrics`**: The current `record_metrics()` is called
   from the collector thread at the end of `perform_multi_threaded_collect()`,
   `perform_single_threaded_collect_with_wake()`, etc. This is the primary
   integration point for per-collection metrics.

3. **Threshold-based collection triggers**: Collection triggers via
   `default_collect_condition()` using drop-count and young-gen-size heuristics.
   This is **not** a debt-based model. Pacing concerns are handled by
   `IncrementalConfig` for incremental marking.

4. **BiBOP memory layout**: Object sizes are known per-page (`block_size`),
   not approximated by constants.

5. **`IncrementalMarkState` is a process-level singleton**: Accessed via
   `IncrementalMarkState::global()`. This is the natural home for global
   mark-phase metrics.

---

## 3. Design

### 3.1 Extended `GcMetrics`

Add fields for phase timing and incremental marking data. Existing fields are
unchanged; new fields default to zero for non-incremental collections.

```rust
/// Statistics from the most recent garbage collection.
#[derive(Debug, Clone, Copy)]
pub struct GcMetrics {
    // --- existing fields (unchanged) ---
    pub duration: Duration,
    pub bytes_reclaimed: usize,
    pub bytes_surviving: usize,
    pub objects_reclaimed: usize,
    pub objects_surviving: usize,
    pub collection_type: CollectionType,
    pub total_collections: usize,

    // --- phase timing ---
    /// Time spent in the clear phase.
    pub clear_duration: Duration,
    /// Time spent in the mark phase (sum of all slices for incremental).
    pub mark_duration: Duration,
    /// Time spent in the sweep phase.
    pub sweep_duration: Duration,

    // --- incremental marking stats ---
    /// Objects marked during this collection (0 for non-incremental).
    pub objects_marked: usize,
    /// Number of dirty pages scanned (0 for STW major).
    pub dirty_pages_scanned: usize,
    /// Number of incremental slices executed (0 for STW).
    pub slices_executed: usize,
    /// Whether incremental marking fell back to STW.
    pub fallback_occurred: bool,
    /// Reason for fallback, if any.
    pub fallback_reason: FallbackReason,
}
```

**Backward compatibility**: New fields are additive. `GcMetrics::new()` sets
them all to zero/false. `#[non_exhaustive]` is recommended but optional (can
be added later as a semver-minor change).

### 3.2 `PhaseTimer` (Internal)

A small helper to capture phase durations without duplicating timing code
across the many `collect_*` functions:

```rust
/// Captures start/end times for GC phases. Internal only.
struct PhaseTimer {
    clear: Duration,
    mark: Duration,
    sweep: Duration,
    current_start: Option<Instant>,
}

impl PhaseTimer {
    fn new() -> Self { ... }
    fn start(&mut self) { self.current_start = Some(Instant::now()); }
    fn end_clear(&mut self) { self.clear = self.current_start.take().unwrap().elapsed(); }
    fn end_mark(&mut self)  { self.mark = self.current_start.take().unwrap().elapsed(); }
    fn end_sweep(&mut self) { self.sweep = self.current_start.take().unwrap().elapsed(); }
}
```

### 3.3 `GlobalMetrics` (Process-Level Cumulative Counters)

A static singleton for cumulative statistics. All fields are atomic for
thread-safe access from any thread.

```rust
/// Cumulative GC statistics across all threads and collections.
///
/// Accessed via `global_metrics()`. All counters use `Relaxed` ordering —
/// they are informational and do not synchronize other state.
pub struct GlobalMetrics {
    /// Total number of GC cycles completed (all types).
    total_collections: AtomicUsize,
    /// Total number of minor collections.
    total_minor_collections: AtomicUsize,
    /// Total number of major collections.
    total_major_collections: AtomicUsize,
    /// Total number of incremental major collections.
    total_incremental_collections: AtomicUsize,
    /// Total bytes reclaimed across all collections.
    total_bytes_reclaimed: AtomicUsize,
    /// Total objects reclaimed across all collections.
    total_objects_reclaimed: AtomicUsize,
    /// Total time spent in GC pauses (nanoseconds).
    total_pause_ns: AtomicU64,
    /// Total number of STW fallbacks from incremental marking.
    total_fallbacks: AtomicUsize,
}
```

**Singleton access:**

```rust
static GLOBAL_METRICS: GlobalMetrics = GlobalMetrics::new();

/// Get cumulative GC statistics.
#[must_use]
pub fn global_metrics() -> &'static GlobalMetrics { &GLOBAL_METRICS }
```

**Recording**: Updated inside `record_metrics()` — the single choke-point
that all collection paths already call:

```rust
pub fn record_metrics(metrics: GcMetrics) {
    // Thread-local: last GC snapshot (existing)
    TOTAL_COLLECTIONS.with(|c| c.set(c.get() + 1));
    LAST_METRICS.with(|m| { /* ... existing code ... */ });

    // Global: cumulative counters (new)
    let g = global_metrics();
    g.total_collections.fetch_add(1, Relaxed);
    g.total_bytes_reclaimed.fetch_add(metrics.bytes_reclaimed, Relaxed);
    g.total_objects_reclaimed.fetch_add(metrics.objects_reclaimed, Relaxed);
    g.total_pause_ns.fetch_add(metrics.duration.as_nanos() as u64, Relaxed);
    match metrics.collection_type {
        CollectionType::Minor => { g.total_minor_collections.fetch_add(1, Relaxed); }
        CollectionType::Major => { g.total_major_collections.fetch_add(1, Relaxed); }
        CollectionType::IncrementalMajor => {
            g.total_incremental_collections.fetch_add(1, Relaxed);
            if metrics.fallback_occurred {
                g.total_fallbacks.fetch_add(1, Relaxed);
            }
        }
        CollectionType::None => {}
    }
}
```

**Public read accessors** (all `#[inline]`, all `Relaxed` ordering):

```rust
impl GlobalMetrics {
    pub fn total_collections(&self) -> usize;
    pub fn total_minor_collections(&self) -> usize;
    pub fn total_major_collections(&self) -> usize;
    pub fn total_incremental_collections(&self) -> usize;
    pub fn total_bytes_reclaimed(&self) -> usize;
    pub fn total_objects_reclaimed(&self) -> usize;
    pub fn total_pause_time(&self) -> Duration;
    pub fn total_fallbacks(&self) -> usize;
}
```

### 3.4 Real-Time Heap Queries

Thin wrappers that read from the existing `LocalHeap` via the `HEAP`
thread-local:

```rust
/// Current total allocated bytes on this thread's heap.
#[must_use]
pub fn current_heap_size() -> usize {
    crate::heap::HEAP
        .try_with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated())
        .unwrap_or(0)
}

/// Current young generation bytes on this thread's heap.
#[must_use]
pub fn current_young_size() -> usize {
    crate::heap::HEAP
        .try_with(|h| unsafe { &*h.tcb.heap.get() }.young_allocated())
        .unwrap_or(0)
}

/// Current old generation bytes on this thread's heap.
#[must_use]
pub fn current_old_size() -> usize {
    crate::heap::HEAP
        .try_with(|h| unsafe { &*h.tcb.heap.get() }.old_allocated())
        .unwrap_or(0)
}
```

> **Note**: These report per-thread heap sizes because `LocalHeap` is
> thread-local. A `global_heap_size()` would require aggregating across all
> threads (via the thread registry), which requires locking. This can be
> added later if needed, but the lock acquisition makes it unsuitable as an
> inline hot-path function.

### 3.5 GC History Ring Buffer (P2)

A fixed-size ring buffer storing the last N collection snapshots. This enables
computing averages, percentiles, and detecting regressions.

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::cell::UnsafeCell;

const HISTORY_SIZE: usize = 64;

/// Ring buffer of recent GC metrics.
///
/// Thread-safety: Writes happen only from `record_metrics()` which is called
/// from the collector thread under the GC handshake (no concurrent writes).
/// Reads may race with writes but always see a complete `GcMetrics` because
/// the write index advances atomically after the slot is fully written.
pub struct GcHistory {
    buffer: UnsafeCell<[GcMetrics; HISTORY_SIZE]>,
    /// Next write position (monotonically increasing).
    write_idx: AtomicUsize,
}

// SAFETY: Writes are serialized by the GC handshake.
// Reads see either the old or new value for a slot; both are valid GcMetrics.
unsafe impl Sync for GcHistory {}

impl GcHistory {
    const fn new() -> Self {
        Self {
            buffer: UnsafeCell::new([GcMetrics::new(); HISTORY_SIZE]),
            write_idx: AtomicUsize::new(0),
        }
    }

    /// Record a new metrics snapshot.
    ///
    /// Called from `record_metrics()` — single-writer context.
    fn push(&self, metrics: GcMetrics) {
        let idx = self.write_idx.load(Ordering::Relaxed) % HISTORY_SIZE;
        // SAFETY: Single writer (GC handshake guarantees exclusivity).
        unsafe { (*self.buffer.get())[idx] = metrics; }
        // Advance index AFTER writing the slot.
        self.write_idx.fetch_add(1, Ordering::Release);
    }

    /// Number of collections recorded (may exceed HISTORY_SIZE).
    pub fn total_recorded(&self) -> usize {
        self.write_idx.load(Ordering::Acquire)
    }

    /// Iterate over recent metrics (up to HISTORY_SIZE, newest first).
    pub fn recent(&self, n: usize) -> Vec<GcMetrics> {
        let total = self.total_recorded();
        let count = n.min(total).min(HISTORY_SIZE);
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let idx = (total - 1 - i) % HISTORY_SIZE;
            // SAFETY: idx is always in bounds; slot was written before
            // write_idx was incremented.
            result.push(unsafe { (*self.buffer.get())[idx] });
        }
        result
    }

    /// Average pause time over the last `n` collections.
    pub fn average_pause_time(&self, n: usize) -> Duration {
        let recent = self.recent(n);
        if recent.is_empty() {
            return Duration::ZERO;
        }
        let total_ns: u128 = recent.iter().map(|m| m.duration.as_nanos()).sum();
        Duration::from_nanos((total_ns / recent.len() as u128) as u64)
    }

    /// Maximum pause time over the last `n` collections.
    pub fn max_pause_time(&self, n: usize) -> Duration {
        self.recent(n)
            .iter()
            .map(|m| m.duration)
            .max()
            .unwrap_or(Duration::ZERO)
    }
}

static GC_HISTORY: GcHistory = GcHistory::new();

/// Get GC collection history.
#[must_use]
pub fn gc_history() -> &'static GcHistory { &GC_HISTORY }
```

---

## 4. Integration Points

This section shows exactly where in the existing code the new metrics get
populated. This is critical — metrics structs without callers are useless.

### 4.1 Phase Timing in Collection Functions

Every collection path in `gc/gc.rs` follows the same Clear → Mark → Sweep
structure. A `PhaseTimer` is threaded through each phase.

**Example: `perform_multi_threaded_collect()`** (lines 283–492 of `gc/gc.rs`):

```rust
fn perform_multi_threaded_collect() {
    let start = Instant::now();
    let mut timer = PhaseTimer::new();

    // ... existing setup ...

    if total_size > MAJOR_THRESHOLD {
        // Phase 1: Clear
        timer.start();
        for tcb in &tcbs { /* clear_all_marks_and_dirty */ }
        timer.end_clear();

        // Phase 2: Mark
        timer.start();
        for tcb in &tcbs { /* mark_major_roots_multi */ }
        timer.end_mark();

        // Phase 3: Sweep
        timer.start();
        for tcb in &tcbs { /* sweep */ }
        timer.end_sweep();
    } else {
        // Minor: no clear phase
        timer.start();
        /* mark + sweep combined in collect_minor_multi */
        timer.end_sweep();
    }

    // Build extended GcMetrics
    let mark_stats = IncrementalMarkState::global().stats();
    crate::metrics::record_metrics(GcMetrics {
        duration: start.elapsed(),
        clear_duration: timer.clear,
        mark_duration: timer.mark,
        sweep_duration: timer.sweep,
        objects_marked: mark_stats.objects_marked.load(Relaxed),
        dirty_pages_scanned: mark_stats.dirty_pages_scanned.load(Relaxed),
        slices_executed: mark_stats.slices_executed.load(Relaxed),
        fallback_occurred: mark_stats.fallback_occurred.load(Relaxed),
        fallback_reason: FallbackReason::from_u32(
            mark_stats.fallback_reason.load(Relaxed)
        ),
        // ... existing fields ...
    });
}
```

The same pattern applies to:
- `perform_single_threaded_collect_with_wake()`
- `perform_single_threaded_collect_full()`
- `perform_multi_threaded_collect_full()`
- `collect_minor()` (single-threaded)
- `collect_major_stw()` / `collect_major_incremental()`

### 4.2 `record_metrics` as the Aggregation Point

All collection paths already converge on `record_metrics()`. This function
becomes the single point where:
1. Thread-local `LAST_METRICS` is updated (existing)
2. `GLOBAL_METRICS` cumulative counters are incremented (new)
3. `GC_HISTORY` ring buffer receives a snapshot (new, P2)

No other call sites are needed.

### 4.3 Heap Queries — No Integration Needed

`current_heap_size()` etc. directly read existing `LocalHeap` fields via the
`HEAP` thread-local. No changes to heap code required.

---

## 5. Public API Summary

### New in `metrics` Module

```rust
// --- Extended GcMetrics (Phase 1) ---
// Fields added to existing GcMetrics struct (see §3.1)

// --- Global cumulative stats (Phase 2) ---
pub fn global_metrics() -> &'static GlobalMetrics;

impl GlobalMetrics {
    pub fn total_collections(&self) -> usize;
    pub fn total_minor_collections(&self) -> usize;
    pub fn total_major_collections(&self) -> usize;
    pub fn total_incremental_collections(&self) -> usize;
    pub fn total_bytes_reclaimed(&self) -> usize;
    pub fn total_objects_reclaimed(&self) -> usize;
    pub fn total_pause_time(&self) -> Duration;
    pub fn total_fallbacks(&self) -> usize;
}

// --- Real-time heap queries (Phase 2) ---
pub fn current_heap_size() -> usize;
pub fn current_young_size() -> usize;
pub fn current_old_size() -> usize;

// --- History (Phase 3, P2) ---
pub fn gc_history() -> &'static GcHistory;

impl GcHistory {
    pub fn total_recorded(&self) -> usize;
    pub fn recent(&self, n: usize) -> Vec<GcMetrics>;
    pub fn average_pause_time(&self, n: usize) -> Duration;
    pub fn max_pause_time(&self, n: usize) -> Duration;
}
```

### Re-exports in `lib.rs`

```rust
pub use metrics::{
    last_gc_metrics, global_metrics, gc_history,
    current_heap_size, current_young_size, current_old_size,
    CollectionType, GcMetrics, GlobalMetrics, GcHistory,
};
```

### Unchanged

- `last_gc_metrics()` — still returns the thread-local last-GC snapshot
- `record_metrics()` — still the internal recording function (but does more)
- `CollectionType` — unchanged
- `IncrementalConfig` — unchanged (metrics don't alter collection behavior)

---

## 6. Usage Examples

These use rudo-gc's actual API:

```rust
use rudo_gc::{Gc, Trace, collect_full, safepoint};
use rudo_gc::metrics;

#[derive(Trace)]
struct Node {
    value: i32,
    next: Option<Gc<Node>>,
}

fn monitor_gc() {
    // Allocate some objects
    let _root = Gc::new(Node { value: 1, next: None });
    for i in 0..1000 {
        let _ = Gc::new(i);
    }

    // Force a collection
    collect_full();

    // --- Per-collection snapshot ---
    let m = metrics::last_gc_metrics();
    println!("Last GC: {:?} ({:?})", m.duration, m.collection_type);
    println!("  Clear: {:?}, Mark: {:?}, Sweep: {:?}",
             m.clear_duration, m.mark_duration, m.sweep_duration);
    println!("  Reclaimed: {} bytes / {} objects",
             m.bytes_reclaimed, m.objects_reclaimed);
    if m.slices_executed > 0 {
        println!("  Incremental: {} slices, {} dirty pages scanned",
                 m.slices_executed, m.dirty_pages_scanned);
        if m.fallback_occurred {
            println!("  Fallback: {:?}", m.fallback_reason);
        }
    }

    // --- Cumulative stats ---
    let g = metrics::global_metrics();
    println!("Total collections: {} (minor={}, major={}, incremental={})",
             g.total_collections(),
             g.total_minor_collections(),
             g.total_major_collections(),
             g.total_incremental_collections());
    println!("Total pause time: {:?}", g.total_pause_time());
    println!("Total reclaimed: {} bytes", g.total_bytes_reclaimed());

    // --- Real-time heap info ---
    println!("Current heap: {} bytes (young={}, old={})",
             metrics::current_heap_size(),
             metrics::current_young_size(),
             metrics::current_old_size());

    // --- History ---
    let h = metrics::gc_history();
    println!("Avg pause (last 10): {:?}", h.average_pause_time(10));
    println!("Max pause (last 10): {:?}", h.max_pause_time(10));
}
```

---

## 7. Implementation Plan

### Phase 1: Extend `GcMetrics` + Phase Timing (P0)

**Files changed**: `metrics.rs`, `gc/gc.rs`

| Step | Task | Complexity |
|------|------|------------|
| 1a | Add new fields to `GcMetrics` and `GcMetrics::new()` | Low |
| 1b | Add `PhaseTimer` internal helper | Low |
| 1c | Thread `PhaseTimer` through `perform_multi_threaded_collect()` | Medium |
| 1d | Thread `PhaseTimer` through `perform_multi_threaded_collect_full()` | Medium |
| 1e | Thread `PhaseTimer` through `perform_single_threaded_collect_with_wake()` | Medium |
| 1f | Thread `PhaseTimer` through `perform_single_threaded_collect_full()` | Medium |
| 1g | Thread `PhaseTimer` through `collect_minor()` | Low |
| 1h | Thread `PhaseTimer` through `collect_major_stw()` | Low |
| 1i | Read `MarkStats` atomics in `collect_major_incremental()` and populate extended fields | Medium |
| 1j | Add `FallbackReason` re-export to `metrics.rs` (or re-export from `incremental`) | Low |
| 1k | Update `lib.rs` re-exports | Low |
| 1l | Tests | Medium |

**Estimated effort**: 8–12 hours

### Phase 2: `GlobalMetrics` + Heap Queries (P1)

**Files changed**: `metrics.rs`, `lib.rs`

| Step | Task | Complexity |
|------|------|------------|
| 2a | Add `GlobalMetrics` struct with atomic fields | Low |
| 2b | Add static singleton and `global_metrics()` accessor | Low |
| 2c | Update `record_metrics()` to increment global counters | Low |
| 2d | Add `current_heap_size()`, `current_young_size()`, `current_old_size()` | Low |
| 2e | Add `GlobalMetrics` read accessors | Low |
| 2f | Update `lib.rs` re-exports | Low |
| 2g | Tests (multi-threaded cumulative counter correctness) | Medium |

**Estimated effort**: 4–6 hours

### Phase 3: GC History Ring Buffer (P2)

**Files changed**: `metrics.rs`, `lib.rs`

| Step | Task | Complexity |
|------|------|------------|
| 3a | Add `GcHistory` struct with `UnsafeCell` buffer | Medium |
| 3b | Implement `push()`, `recent()`, `average_pause_time()`, `max_pause_time()` | Medium |
| 3c | Add static singleton and `gc_history()` accessor | Low |
| 3d | Call `GC_HISTORY.push()` from `record_metrics()` | Low |
| 3e | Update `lib.rs` re-exports | Low |
| 3f | Tests (ring buffer wrap-around, concurrent read safety) | Medium |
| 3g | SAFETY audit for `UnsafeCell` usage | Medium |

**Estimated effort**: 6–10 hours

### Total Estimated Effort: 18–28 hours

---

## 8. Testing Plan

### 8.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_metrics_new_fields_default_to_zero() {
        let m = GcMetrics::new();
        assert_eq!(m.clear_duration, Duration::ZERO);
        assert_eq!(m.mark_duration, Duration::ZERO);
        assert_eq!(m.sweep_duration, Duration::ZERO);
        assert_eq!(m.objects_marked, 0);
        assert_eq!(m.slices_executed, 0);
        assert!(!m.fallback_occurred);
    }

    #[test]
    fn test_global_metrics_accumulate() {
        let g = global_metrics();
        let before = g.total_collections();
        record_metrics(GcMetrics {
            collection_type: CollectionType::Minor,
            bytes_reclaimed: 1024,
            duration: Duration::from_micros(500),
            ..GcMetrics::new()
        });
        assert_eq!(g.total_collections(), before + 1);
        assert!(g.total_bytes_reclaimed() >= 1024);
    }

    #[test]
    fn test_heap_queries_return_sane_values() {
        let _ = crate::Gc::new(42);
        assert!(current_heap_size() > 0);
    }

    #[test]
    fn test_history_ring_buffer() {
        let h = gc_history();
        let before = h.total_recorded();
        collect_full();
        assert!(h.total_recorded() > before);
        let recent = h.recent(1);
        assert!(!recent.is_empty());
        assert!(recent[0].duration > Duration::ZERO);
    }

    #[test]
    fn test_history_average_pause() {
        let h = gc_history();
        // Average of zero collections is Duration::ZERO
        let avg = GcHistory::new().average_pause_time(10);
        assert_eq!(avg, Duration::ZERO);
    }
}
```

### 8.2 Integration Tests

```rust
#[test]
fn test_phase_timing_sums_approximately() {
    // Phase durations should sum to roughly total duration
    // (with some overhead for setup/teardown between phases)
    let _ = crate::Gc::new(42);
    crate::collect_full();
    let m = crate::last_gc_metrics();
    let phase_sum = m.clear_duration + m.mark_duration + m.sweep_duration;
    // Allow 20% overhead for inter-phase work
    assert!(phase_sum <= m.duration + Duration::from_micros(100));
}

#[test]
fn test_incremental_metrics_populated() {
    use rudo_gc::{set_incremental_config, IncrementalConfig};
    set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });
    // Allocate enough to trigger major GC
    for _ in 0..100_000 {
        let _ = crate::Gc::new([0u8; 128]);
    }
    crate::collect_full();
    let m = crate::last_gc_metrics();
    // When incremental is enabled for major GC, these should be populated
    if m.collection_type == crate::CollectionType::IncrementalMajor {
        assert!(m.objects_marked > 0);
        assert!(m.slices_executed > 0);
    }
}

#[test]
fn test_global_metrics_multi_threaded() {
    use std::thread;
    let g = crate::metrics::global_metrics();
    let before = g.total_collections();
    let handles: Vec<_> = (0..4).map(|_| {
        thread::spawn(|| {
            for _ in 0..100 {
                let _ = crate::Gc::new(42);
            }
            crate::collect_full();
        })
    }).collect();
    for h in handles { h.join().unwrap(); }
    assert!(g.total_collections() > before);
}
```

---

## 9. Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| Phase timing adds overhead to every GC | `Instant::now()` is ~20ns on Linux; negligible vs GC pause times (microseconds–milliseconds) |
| `GlobalMetrics` atomic contention | `Relaxed` ordering is ~1 CPU cycle; only written once per GC cycle |
| `GcHistory` `UnsafeCell` soundness | Single-writer (GC handshake), atomic write-index for reader safety; requires SAFETY audit |
| Bloating `GcMetrics` struct | 7 new fields add ~56 bytes; struct is stack-allocated and copied infrequently |
| Breaking change if `#[non_exhaustive]` added | Defer to a later semver-minor release; document intent |

---

## 10. What This Plan Intentionally Excludes

1. **Pacing / allocation debt system**: rudo-gc's collection triggers are
   threshold-based (`default_collect_condition`), not debt-based. Incremental
   marking is controlled by `IncrementalConfig`. Adding a gc-arena-style
   `Pacing` struct would conflict with both systems without a fundamental
   redesign of the collection trigger mechanism.

2. **External allocation tracking**: Useful in arena-based collectors where
   users explicitly manage arena lifetimes. In rudo-gc's global-GC model,
   external allocations don't participate in collection decisions. Can be
   revisited if `CollectInfo` is extended with external-bytes awareness.

3. **Automatic export formats (JSON, Prometheus)**: Out of scope for the core
   crate. Users can build these on top of the public API. A future
   `rudo-gc-metrics` crate could provide these.

4. **Per-thread metrics aggregation**: The `current_heap_size()` family
   reports per-thread data. A global aggregate would require locking the
   thread registry. Deferred to avoid introducing lock contention on a
   read path.

---

## 11. Reference: gc-arena's Approach (for Context)

gc-arena's metrics system is well-designed for its single-threaded,
arena-based, debt-driven model:

- `Metrics` is `Rc<MetricsInner>` — no atomics needed
- `Pacing` struct controls incremental work factors (`mark_factor`,
  `trace_factor`, `keep_factor`, `drop_factor`, `free_factor`)
- `allocation_debt()` drives incremental collection: debt = allocation -
  wakeup_amount + artificial_debt - credits
- Per-cycle stats: `allocated_gc_bytes`, `marked_gcs`, `traced_gcs`,
  `remembered_gcs` (all via `Cell<usize>`)
- External allocation tracking via `mark_external_allocation()` /
  `mark_external_deallocation()`

These concepts are excellent for gc-arena but map poorly onto rudo-gc because:

| gc-arena | rudo-gc |
|----------|---------|
| Single-threaded (`Rc`, `Cell`) | Multi-threaded (`AtomicUsize`, per-thread heaps) |
| Arena-scoped, user calls `collect_debt()` | Global GC, automatic threshold-based triggers |
| Debt-based incremental pacing | `IncrementalConfig` with budget/timeout parameters |
| One root, explicit `mutate()` callbacks | Conservative stack scanning, no arena API |

The plan above takes the **spirit** of gc-arena's observability (cumulative
stats, per-cycle breakdown, real-time queries) and adapts the **mechanism**
to rudo-gc's architecture.

---

## 12. Change History

| Version | Date | Notes |
|---------|------|-------|
| v2.0 | 2026-02-06 | Rewritten from scratch for rudo-gc architecture |
