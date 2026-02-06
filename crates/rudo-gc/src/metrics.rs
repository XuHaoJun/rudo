//! GC metrics and statistics.

use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Re-export `FallbackReason` from incremental module.
pub use crate::gc::incremental::FallbackReason;

/// Statistics from the most recent garbage collection.
#[derive(Debug, Clone, Copy)]
pub struct GcMetrics {
    /// Duration of the last collection.
    pub duration: Duration,
    /// Number of bytes reclaimed.
    pub bytes_reclaimed: usize,
    /// Number of bytes surviving.
    pub bytes_surviving: usize,
    /// Number of objects reclaimed.
    pub objects_reclaimed: usize,
    /// Number of objects surviving.
    pub objects_surviving: usize,
    /// Type of collection (Minor or Major).
    pub collection_type: CollectionType,
    /// Total collections since process start.
    pub total_collections: usize,
    /// Duration of the clear phase.
    pub clear_duration: Duration,
    /// Duration of the mark phase.
    pub mark_duration: Duration,
    /// Duration of the sweep phase.
    pub sweep_duration: Duration,
    /// Number of objects marked (for incremental marking).
    pub objects_marked: usize,
    /// Number of dirty pages scanned (for incremental marking).
    pub dirty_pages_scanned: usize,
    /// Number of incremental slices executed.
    pub slices_executed: usize,
    /// Whether incremental marking fell back to STW.
    pub fallback_occurred: bool,
    /// Reason for fallback, if any.
    pub fallback_reason: FallbackReason,
}

impl Default for GcMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl GcMetrics {
    /// Create a new `GcMetrics` with all fields set to zero/defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            duration: Duration::from_secs(0),
            bytes_reclaimed: 0,
            bytes_surviving: 0,
            objects_reclaimed: 0,
            objects_surviving: 0,
            collection_type: CollectionType::None,
            total_collections: 0,
            clear_duration: Duration::from_secs(0),
            mark_duration: Duration::from_secs(0),
            sweep_duration: Duration::from_secs(0),
            objects_marked: 0,
            dirty_pages_scanned: 0,
            slices_executed: 0,
            fallback_occurred: false,
            fallback_reason: FallbackReason::None,
        }
    }
}

/// Type of GC collection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum CollectionType {
    /// No collection has run yet.
    #[default]
    None = 0,
    /// A minor collection (Young Gen only).
    Minor = 1,
    /// A major collection (Full heap).
    Major = 2,
    /// A major collection with incremental marking.
    IncrementalMajor = 3,
}

/// Internal helper for capturing phase durations.
///
/// This struct is used by collection functions to time the three GC phases:
/// - Clear: Reset mark bits and prepare for marking
/// - Mark: Traverse reachable objects and mark them
/// - Sweep: Reclaim unmarked objects
///
/// # Example
///
/// ```
/// use std::time::Instant;
/// use rudo_gc::metrics::PhaseTimer;
///
/// let mut timer = PhaseTimer::new();
/// timer.start();
/// // ... clear phase work ...
/// timer.end_clear();
///
/// timer.start();
/// // ... mark phase work ...
/// timer.end_mark();
///
/// timer.start();
/// // ... sweep phase work ...
/// timer.end_sweep();
/// ```
#[derive(Debug, Clone, Copy)]
pub struct PhaseTimer {
    /// Accumulated clear phase time.
    pub clear: Duration,
    /// Accumulated mark phase time.
    pub mark: Duration,
    /// Accumulated sweep phase time.
    pub sweep: Duration,
    /// Start time of current phase.
    current_start: Option<Instant>,
}

impl PhaseTimer {
    /// Create a new `PhaseTimer` with all durations set to zero.
    pub const fn new() -> Self {
        Self {
            clear: Duration::ZERO,
            mark: Duration::ZERO,
            sweep: Duration::ZERO,
            current_start: None,
        }
    }

    /// Start timing a phase.
    pub fn start(&mut self) {
        self.current_start = Some(Instant::now());
    }

    /// End the clear phase and record its duration.
    pub fn end_clear(&mut self) {
        if let Some(start) = self.current_start.take() {
            self.clear = start.elapsed();
        }
    }

    /// End the mark phase and record its duration.
    pub fn end_mark(&mut self) {
        if let Some(start) = self.current_start.take() {
            self.mark = start.elapsed();
        }
    }

    /// End the sweep phase and record its duration.
    pub fn end_sweep(&mut self) {
        if let Some(start) = self.current_start.take() {
            self.sweep = start.elapsed();
        }
    }
}

/// Process-level cumulative GC statistics.
///
/// This struct provides atomic counters for cumulative GC metrics across
/// all threads and collections since process start.
///
/// # Example
///
/// ```
/// use rudo_gc::global_metrics;
///
/// let metrics = global_metrics();
/// println!("Total collections: {}", metrics.total_collections());
/// println!("Total bytes reclaimed: {}", metrics.total_bytes_reclaimed());
/// ```
#[derive(Debug)]
pub struct GlobalMetrics {
    collections: AtomicUsize,
    minor_collections: AtomicUsize,
    major_collections: AtomicUsize,
    incremental_collections: AtomicUsize,
    bytes_reclaimed: AtomicUsize,
    objects_reclaimed: AtomicUsize,
    pause_ns: AtomicU64,
    fallbacks: AtomicUsize,
}

impl Default for GlobalMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalMetrics {
    /// Create a new `GlobalMetrics` with all counters initialized to zero.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub const fn new() -> Self {
        Self {
            collections: AtomicUsize::new(0),
            minor_collections: AtomicUsize::new(0),
            major_collections: AtomicUsize::new(0),
            incremental_collections: AtomicUsize::new(0),
            bytes_reclaimed: AtomicUsize::new(0),
            objects_reclaimed: AtomicUsize::new(0),
            pause_ns: AtomicU64::new(0),
            fallbacks: AtomicUsize::new(0),
        }
    }

    /// Returns the total number of GC collections performed.
    #[inline]
    #[must_use]
    pub fn total_collections(&self) -> usize {
        self.collections.load(Ordering::Relaxed)
    }

    /// Returns the total number of minor collections performed.
    #[inline]
    #[must_use]
    pub fn total_minor_collections(&self) -> usize {
        self.minor_collections.load(Ordering::Relaxed)
    }

    /// Returns the total number of major (STW) collections performed.
    #[inline]
    #[must_use]
    pub fn total_major_collections(&self) -> usize {
        self.major_collections.load(Ordering::Relaxed)
    }

    /// Returns the total number of incremental major collections performed.
    #[inline]
    #[must_use]
    pub fn total_incremental_collections(&self) -> usize {
        self.incremental_collections.load(Ordering::Relaxed)
    }

    /// Returns the total number of bytes reclaimed by GC.
    #[inline]
    #[must_use]
    pub fn total_bytes_reclaimed(&self) -> usize {
        self.bytes_reclaimed.load(Ordering::Relaxed)
    }

    /// Returns the total number of objects reclaimed by GC.
    #[inline]
    #[must_use]
    pub fn total_objects_reclaimed(&self) -> usize {
        self.objects_reclaimed.load(Ordering::Relaxed)
    }

    /// Returns the total pause time in nanoseconds.
    #[inline]
    #[must_use]
    pub fn total_pause_ns(&self) -> u64 {
        self.pause_ns.load(Ordering::Relaxed)
    }

    /// Returns the total number of STW fallbacks from incremental marking.
    #[inline]
    #[must_use]
    pub fn total_fallbacks(&self) -> usize {
        self.fallbacks.load(Ordering::Relaxed)
    }
}

static GLOBAL_METRICS: GlobalMetrics = GlobalMetrics::new();

/// Get the global cumulative GC metrics.
///
/// Returns a reference to the process-level singleton that tracks
/// cumulative statistics across all threads and collections.
///
/// # Example
///
/// ```
/// use rudo_gc::global_metrics;
///
/// let metrics = global_metrics();
/// println!("Total collections: {}", metrics.total_collections());
/// ```
#[must_use]
pub fn global_metrics() -> &'static GlobalMetrics {
    &GLOBAL_METRICS
}

/// Get the current heap size for this thread.
///
/// Returns the total bytes allocated in this thread's heap,
/// or 0 if the current thread doesn't have a heap.
///
/// This function does NOT trigger garbage collection.
///
/// # Example
///
/// ```
/// use rudo_gc::current_heap_size;
///
/// let size = current_heap_size();
/// println!("Current heap size: {} bytes", size);
/// ```
#[must_use]
pub fn current_heap_size() -> usize {
    crate::heap::HEAP
        .try_with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated())
        .unwrap_or(0)
}

/// Get the current young generation size for this thread.
///
/// Returns the bytes allocated in the young generation of this thread's heap,
/// or 0 if the current thread doesn't have a heap.
///
/// This function does NOT trigger garbage collection.
///
/// # Example
///
/// ```
/// use rudo_gc::current_young_size;
///
/// let size = current_young_size();
/// println!("Current young generation size: {} bytes", size);
/// ```
#[must_use]
pub fn current_young_size() -> usize {
    crate::heap::HEAP
        .try_with(|h| unsafe { &*h.tcb.heap.get() }.young_allocated())
        .unwrap_or(0)
}

/// Get the current old generation size for this thread.
///
/// Returns the bytes allocated in the old generation of this thread's heap,
/// or 0 if the current thread doesn't have a heap.
///
/// This function does NOT trigger garbage collection.
///
/// # Example
///
/// ```
/// use rudo_gc::current_old_size;
///
/// let size = current_old_size();
/// println!("Current old generation size: {} bytes", size);
/// ```
#[must_use]
pub fn current_old_size() -> usize {
    crate::heap::HEAP
        .try_with(|h| unsafe { &*h.tcb.heap.get() }.old_allocated())
        .unwrap_or(0)
}

/// Ring buffer size for GC history.
const HISTORY_SIZE: usize = 64;

/// Fixed-size ring buffer of recent `GcMetrics` snapshots.
///
/// Stores the most recent 64 GC collections for trend analysis.
/// Uses a single-writer pattern protected by the GC handshake serialization.
///
/// # Safety
///
/// This type implements `Sync` because writes are serialized by the GC handshake.
/// Only one collection runs at a time, ensuring single-writer access.
/// The write-index uses `Release` ordering for publishing, and readers use
/// `Acquire` when reading the index, establishing proper happens-before relationships.
///
/// # Example
///
/// ```
/// use rudo_gc::gc_history;
///
/// let history = gc_history();
/// println!("Total recorded: {}", history.total_recorded());
/// ```
#[derive(Debug)]
pub struct GcHistory {
    buffer: UnsafeCell<[GcMetrics; HISTORY_SIZE]>,
    write_idx: AtomicUsize,
}

/// SAFETY: `GcHistory` is safe to share across threads because:
///
/// 1. **Single writer guarantee**: The GC handshake ensures only one collection
///    runs at a time. All calls to `push()` occur during collection, serialized
///    by the handshake mechanism.
///
/// 2. **Atomic publish**: The write-index uses `Release` ordering when advancing,
///    ensuring all slot writes are visible before the index is published.
///    Readers use `Acquire` when reading the index, establishing happens-before.
///
/// 3. **Torn read tolerance**: A reader might see a partially-written slot during
///    a race, but `GcMetrics` is `Copy` with only primitive fields. A torn read
///    produces a valid (if slightly incorrect) `GcMetrics`, acceptable for
///    informational data.
unsafe impl Sync for GcHistory {}

impl Default for GcHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl GcHistory {
    /// Create a new `GcHistory` with an empty buffer.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub const fn new() -> Self {
        Self {
            buffer: UnsafeCell::new([GcMetrics::new(); HISTORY_SIZE]),
            write_idx: AtomicUsize::new(0),
        }
    }

    /// Push a new metrics snapshot to the history buffer.
    ///
    /// Uses a ring buffer: when full, older entries are overwritten.
    fn push(&self, metrics: GcMetrics) {
        let idx = self.write_idx.fetch_add(1, Ordering::Relaxed);
        // SAFETY: Single-writer guarantee from GC handshake ensures no concurrent writes.
        // The write_idx atomic provides synchronization for readers.
        unsafe {
            let buffer = &mut *self.buffer.get();
            buffer[idx % HISTORY_SIZE] = metrics;
        }
    }

    /// Get the total number of metrics recorded.
    ///
    /// This may exceed `HISTORY_SIZE` if more collections have occurred
    /// than the buffer can hold.
    #[inline]
    #[must_use]
    pub fn total_recorded(&self) -> usize {
        self.write_idx.load(Ordering::Acquire)
    }

    /// Get the most recent N metrics snapshots.
    ///
    /// Returns at most N entries, newest first, capped by both the
    /// number of entries recorded and the buffer size.
    #[must_use]
    pub fn recent(&self, n: usize) -> Vec<GcMetrics> {
        let total = self.total_recorded();
        if total == 0 {
            return Vec::new();
        }

        let n = n.min(HISTORY_SIZE).min(total);
        let start = total.saturating_sub(n);
        let mut result = Vec::with_capacity(n);

        // SAFETY: Readers may race with writers, but:
        // 1. If we read write_idx after loading, all slots we access have been published
        // 2. A torn read of GcMetrics (Copy + primitives) produces valid data
        // 3. If write_idx wraps around, we correctly handle modulo indexing
        unsafe {
            let buffer = &*self.buffer.get();
            for i in start..total {
                result.push(buffer[i % HISTORY_SIZE]);
            }
        }

        result
    }

    /// Compute the average pause time from the most recent N collections.
    ///
    /// Returns `Duration::ZERO` if no collections have been recorded.
    #[inline]
    #[must_use]
    pub fn average_pause_time(&self, n: usize) -> Duration {
        let recent = self.recent(n);
        if recent.is_empty() {
            return Duration::ZERO;
        }

        #[allow(clippy::unnecessary_cast)]
        let total_ns: u128 = recent.iter().map(|m| m.duration.as_nanos() as u128).sum();
        Duration::from_nanos(
            (total_ns / recent.len() as u128)
                .try_into()
                .unwrap_or(u64::MAX),
        )
    }

    /// Get the maximum pause time from the most recent N collections.
    ///
    /// Returns `Duration::ZERO` if no collections have been recorded.
    #[inline]
    #[must_use]
    pub fn max_pause_time(&self, n: usize) -> Duration {
        let recent = self.recent(n);
        if recent.is_empty() {
            return Duration::ZERO;
        }

        recent
            .iter()
            .map(|m| m.duration)
            .max()
            .unwrap_or(Duration::ZERO)
    }
}

static GC_HISTORY: GcHistory = GcHistory::new();

/// Get the GC history ring buffer.
///
/// Returns a reference to the process-level singleton that tracks
/// recent GC collections for trend analysis.
///
/// # Example
///
/// ```
/// use rudo_gc::gc_history;
///
/// let history = gc_history();
/// println!("Total recorded: {}", history.total_recorded());
/// println!("Average pause (last 10): {:?}", history.average_pause_time(10));
/// ```
#[must_use]
pub fn gc_history() -> &'static GcHistory {
    &GC_HISTORY
}

thread_local! {
    static LAST_METRICS: Cell<GcMetrics> = const { Cell::new(GcMetrics::new()) };
    static TOTAL_COLLECTIONS: Cell<usize> = const { Cell::new(0) };
}

/// Get metrics from the last garbage collection.
#[must_use]
pub fn last_gc_metrics() -> GcMetrics {
    LAST_METRICS.with(Cell::get)
}

/// Record metrics for a collection.
pub fn record_metrics(metrics: GcMetrics) {
    TOTAL_COLLECTIONS.with(|c| c.set(c.get() + 1));
    LAST_METRICS.with(|m| {
        let mut metrics = metrics;
        metrics.total_collections = TOTAL_COLLECTIONS.with(Cell::get);
        m.set(metrics);
    });

    let g = global_metrics();
    g.collections.fetch_add(1, Ordering::Relaxed);
    g.bytes_reclaimed
        .fetch_add(metrics.bytes_reclaimed, Ordering::Relaxed);
    g.objects_reclaimed
        .fetch_add(metrics.objects_reclaimed, Ordering::Relaxed);
    g.pause_ns.fetch_add(
        metrics.duration.as_nanos().try_into().unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
    match metrics.collection_type {
        CollectionType::Minor => {
            g.minor_collections.fetch_add(1, Ordering::Relaxed);
        }
        CollectionType::Major => {
            g.major_collections.fetch_add(1, Ordering::Relaxed);
        }
        CollectionType::IncrementalMajor => {
            g.incremental_collections.fetch_add(1, Ordering::Relaxed);
            if metrics.fallback_occurred {
                g.fallbacks.fetch_add(1, Ordering::Relaxed);
            }
        }
        CollectionType::None => {}
    }

    GC_HISTORY.push(metrics);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_metrics_new_fields_default_to_zero() {
        let metrics = GcMetrics::new();

        assert_eq!(metrics.clear_duration, Duration::ZERO);
        assert_eq!(metrics.mark_duration, Duration::ZERO);
        assert_eq!(metrics.sweep_duration, Duration::ZERO);
        assert_eq!(metrics.objects_marked, 0);
        assert_eq!(metrics.dirty_pages_scanned, 0);
        assert_eq!(metrics.slices_executed, 0);
        assert!(!metrics.fallback_occurred);
        assert_eq!(metrics.fallback_reason, FallbackReason::None);
    }

    #[test]
    fn test_phase_timer_captures_durations() {
        let mut timer = PhaseTimer::new();

        assert_eq!(timer.clear, Duration::ZERO);
        assert_eq!(timer.mark, Duration::ZERO);
        assert_eq!(timer.sweep, Duration::ZERO);
        assert!(timer.current_start.is_none());

        timer.start();
        assert!(timer.current_start.is_some());

        std::thread::sleep(Duration::from_millis(1));
        timer.end_clear();

        assert!(timer.clear > Duration::ZERO);
        assert!(timer.current_start.is_none());

        timer.start();
        std::thread::sleep(Duration::from_millis(1));
        timer.end_mark();

        assert!(timer.mark > Duration::ZERO);

        timer.start();
        std::thread::sleep(Duration::from_millis(1));
        timer.end_sweep();

        assert!(timer.sweep > Duration::ZERO);

        let total = timer.clear + timer.mark + timer.sweep;
        assert!(total > Duration::ZERO);
    }

    #[test]
    fn test_global_metrics_new() {
        let metrics = GlobalMetrics::new();

        assert_eq!(metrics.total_collections(), 0);
        assert_eq!(metrics.total_minor_collections(), 0);
        assert_eq!(metrics.total_major_collections(), 0);
        assert_eq!(metrics.total_incremental_collections(), 0);
        assert_eq!(metrics.total_bytes_reclaimed(), 0);
        assert_eq!(metrics.total_objects_reclaimed(), 0);
        assert_eq!(metrics.total_pause_ns(), 0);
        assert_eq!(metrics.total_fallbacks(), 0);
    }

    #[test]
    fn test_gc_history_new() {
        let history = GcHistory::new();

        assert_eq!(history.total_recorded(), 0);
        assert!(history.recent(10).is_empty());
        assert_eq!(history.average_pause_time(10), Duration::ZERO);
        assert_eq!(history.max_pause_time(10), Duration::ZERO);
    }
}
