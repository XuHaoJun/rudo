# API Contract: Extended GC Metrics System

**Branch**: `010-gc-metrics-v2` | **Date**: 2026-02-06

## Module: `rudo_gc::metrics`

All items below are public API surface. Internal items (`PhaseTimer`, `CollectResult`) are omitted.

---

## 1. Extended `GcMetrics` Struct

```rust
/// Statistics from the most recent garbage collection.
///
/// # Example
///
/// ```rust
/// use rudo_gc::{Gc, collect_full, last_gc_metrics};
///
/// let _obj = Gc::new(42);
/// collect_full();
/// let m = last_gc_metrics();
/// println!("GC took {:?} (clear={:?}, mark={:?}, sweep={:?})",
///          m.duration, m.clear_duration, m.mark_duration, m.sweep_duration);
/// ```
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

    // --- NEW: phase timing ---
    /// Time spent in the clear phase. Zero for minor collections.
    pub clear_duration: Duration,
    /// Time spent in the mark phase. For incremental collections,
    /// this is the sum of all marking slices.
    pub mark_duration: Duration,
    /// Time spent in the sweep phase.
    pub sweep_duration: Duration,

    // --- NEW: incremental marking stats ---
    /// Objects marked during this collection. Zero for non-incremental.
    pub objects_marked: usize,
    /// Number of dirty pages scanned. Zero for STW major.
    pub dirty_pages_scanned: usize,
    /// Number of incremental slices executed. Zero for STW.
    pub slices_executed: usize,
    /// Whether incremental marking fell back to STW.
    pub fallback_occurred: bool,
    /// Reason for fallback, if any.
    pub fallback_reason: FallbackReason,
}
```

### `GcMetrics::new()` (existing, updated)

```rust
/// Create a new `GcMetrics` with all fields set to zero/defaults.
///
/// New fields default to: `Duration::ZERO`, `0`, `false`, `FallbackReason::None`.
#[must_use]
pub const fn new() -> Self;
```

### `impl Default for GcMetrics` (existing, unchanged)

Delegates to `GcMetrics::new()`.

---

## 2. `FallbackReason` Re-export

```rust
/// Re-exported from `gc::incremental`.
///
/// Reason for incremental marking falling back to stop-the-world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FallbackReason {
    None = 0,
    DirtyPagesExceeded = 1,
    SliceTimeout = 2,
    WorklistUnbounded = 3,
    SatbBufferOverflow = 4,
}
```

Already exists in `gc::incremental`. New: re-export via `metrics` module and `lib.rs`.

---

## 3. `GlobalMetrics` Struct

```rust
/// Cumulative GC statistics across all threads and collections.
///
/// Accessed via [`global_metrics()`]. All counters use `Relaxed` ordering —
/// they are informational and do not synchronize other state.
///
/// # Example
///
/// ```rust
/// use rudo_gc::metrics::global_metrics;
///
/// let g = global_metrics();
/// println!("Total collections: {} (minor={}, major={}, incremental={})",
///          g.total_collections(),
///          g.total_minor_collections(),
///          g.total_major_collections(),
///          g.total_incremental_collections());
/// println!("Total pause time: {:?}", g.total_pause_time());
/// ```
pub struct GlobalMetrics { /* atomic fields, not public */ }
```

### Read Accessors

All `#[inline]`, all `#[must_use]`, all use `Relaxed` ordering.

```rust
impl GlobalMetrics {
    /// Total number of GC cycles completed (all types).
    pub fn total_collections(&self) -> usize;

    /// Total number of minor collections.
    pub fn total_minor_collections(&self) -> usize;

    /// Total number of major (STW) collections.
    pub fn total_major_collections(&self) -> usize;

    /// Total number of incremental major collections.
    pub fn total_incremental_collections(&self) -> usize;

    /// Total bytes reclaimed across all collections.
    pub fn total_bytes_reclaimed(&self) -> usize;

    /// Total objects reclaimed across all collections.
    pub fn total_objects_reclaimed(&self) -> usize;

    /// Total time spent in GC pauses.
    pub fn total_pause_time(&self) -> Duration;

    /// Total number of STW fallbacks from incremental marking.
    pub fn total_fallbacks(&self) -> usize;
}
```

### Singleton Accessor

```rust
/// Get cumulative GC statistics.
///
/// Returns a reference to the process-level `GlobalMetrics` singleton.
/// All accessor methods are lock-free and can be called from any thread.
#[must_use]
pub fn global_metrics() -> &'static GlobalMetrics;
```

---

## 4. Real-Time Heap Queries

Per-thread heap size queries. Read directly from the thread-local `LocalHeap`. No GC triggered.

```rust
/// Current total allocated bytes on this thread's heap.
///
/// Returns `0` if the thread has not initialized a heap.
///
/// # Example
///
/// ```rust
/// use rudo_gc::{Gc, metrics};
///
/// let _obj = Gc::new(42);
/// println!("Heap: {} bytes", metrics::current_heap_size());
/// ```
#[must_use]
pub fn current_heap_size() -> usize;

/// Current young generation bytes on this thread's heap.
///
/// Returns `0` if the thread has not initialized a heap.
#[must_use]
pub fn current_young_size() -> usize;

/// Current old generation bytes on this thread's heap.
///
/// Returns `0` if the thread has not initialized a heap.
#[must_use]
pub fn current_old_size() -> usize;
```

**Note**: These report per-thread heap sizes. A cross-thread aggregate is not provided (requires locking the thread registry).

---

## 5. `GcHistory` Struct

```rust
/// History of recent GC metrics for trend analysis.
///
/// Stores the last 64 collection snapshots in a ring buffer.
/// Thread-safe for concurrent reads from any thread.
///
/// # Example
///
/// ```rust
/// use rudo_gc::metrics::gc_history;
///
/// let h = gc_history();
/// println!("Collections recorded: {}", h.total_recorded());
/// println!("Avg pause (last 10): {:?}", h.average_pause_time(10));
/// println!("Max pause (last 10): {:?}", h.max_pause_time(10));
///
/// for m in h.recent(5) {
///     println!("  {:?} - {:?}", m.collection_type, m.duration);
/// }
/// ```
pub struct GcHistory { /* internal fields */ }
```

### Methods

```rust
impl GcHistory {
    /// Number of collections recorded (may exceed buffer capacity).
    ///
    /// This is a monotonically increasing counter. If it exceeds the
    /// buffer capacity (64), only the most recent entries are retained.
    pub fn total_recorded(&self) -> usize;

    /// Get the `n` most recent metrics snapshots, newest first.
    ///
    /// Returns at most `min(n, total_recorded, 64)` entries.
    /// Returns an empty `Vec` if no collections have occurred.
    pub fn recent(&self, n: usize) -> Vec<GcMetrics>;

    /// Average pause time over the last `n` collections.
    ///
    /// Returns `Duration::ZERO` if no collections have occurred.
    pub fn average_pause_time(&self, n: usize) -> Duration;

    /// Maximum pause time over the last `n` collections.
    ///
    /// Returns `Duration::ZERO` if no collections have occurred.
    pub fn max_pause_time(&self, n: usize) -> Duration;
}
```

### Singleton Accessor

```rust
/// Get GC collection history.
///
/// Returns a reference to the process-level `GcHistory` singleton.
/// All methods are lock-free and can be called from any thread.
#[must_use]
pub fn gc_history() -> &'static GcHistory;
```

---

## 6. Existing Functions (Unchanged)

```rust
/// Get metrics from the last garbage collection on this thread.
#[must_use]
pub fn last_gc_metrics() -> GcMetrics;

/// Record metrics for a collection. (Internal, called from collection functions.)
///
/// Updated to also:
/// - Increment `GLOBAL_METRICS` counters
/// - Push to `GC_HISTORY` ring buffer
pub fn record_metrics(metrics: GcMetrics);
```

---

## 7. Re-exports in `lib.rs`

```rust
pub use metrics::{
    // Existing
    last_gc_metrics, CollectionType, GcMetrics,
    // New
    global_metrics, gc_history,
    current_heap_size, current_young_size, current_old_size,
    GlobalMetrics, GcHistory, FallbackReason,
};
```

---

## 8. Thread Safety Summary

| Item | Write Safety | Read Safety |
|------|-------------|-------------|
| `GcMetrics` in `LAST_METRICS` | Thread-local `Cell` (single thread) | Thread-local `Cell` (single thread) |
| `GlobalMetrics` | `AtomicUsize`/`AtomicU64` with `Relaxed` | Same atomics with `Relaxed` |
| `GcHistory` | Single writer (GC handshake) + atomic index | `Acquire` on index read |
| Heap queries | N/A (read-only) | Thread-local `HEAP` access |

---

## 9. Backward Compatibility

| Aspect | Impact |
|--------|--------|
| `GcMetrics` struct | **Additive only** — new fields with zero defaults. Existing code using struct literals must be updated (add `..GcMetrics::new()` for new fields). This affects only internal code (collection functions). |
| `GcMetrics::new()` | Updated to include new fields with zero defaults. |
| `last_gc_metrics()` | Unchanged signature. Returns extended `GcMetrics`. |
| `record_metrics()` | Unchanged signature. Internal behavior extended. |
| `CollectionType` | Unchanged. `IncrementalMajor` variant already exists but was unused. |
| `lib.rs` re-exports | Additive only — new items exported. |
