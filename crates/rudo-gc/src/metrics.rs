//! GC metrics and statistics.

use std::cell::Cell;
use std::time::Duration;

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
}
