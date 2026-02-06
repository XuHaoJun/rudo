//! GC tracing support.
//!
//! When the `tracing` feature is enabled, this module provides structured
//! tracing spans and events for garbage collection operations.

#[cfg(feature = "tracing")]
pub mod internal {
    use std::sync::atomic::{AtomicU64, Ordering};
    use tracing::{span, Level};

    /// High-level GC phases (clear/mark/sweep).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[allow(dead_code)]
    pub enum GcPhase {
        /// Reset mark bits and dirty page tracking.
        Clear,
        /// Trace live object graph.
        Mark,
        /// Reclaim unreachable objects.
        Sweep,
    }

    /// Stable identifier for a GC run.
    ///
    /// This ID is used to correlate all events within a single garbage
    /// collection run. It is a monotonically increasing counter that
    /// starts at 1 and wraps on overflow (which is effectively infinite
    /// for practical GC frequencies).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use rudo_gc::GcId;
    ///
    /// // GcId is Copy and cheap to pass around
    /// let id = get_current_gc_id();
    /// log_custom_metric(id, "heap_size", 1024);
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GcId(pub u64);

    /// Global counter for generating unique GC IDs.
    static NEXT_GC_ID: AtomicU64 = AtomicU64::new(1);

    /// Generate the next unique GC ID.
    pub fn next_gc_id() -> GcId {
        GcId(NEXT_GC_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Create a span for the entire GC collection.
    pub fn trace_gc_collection(collection_type: &str, gc_id: GcId) -> span::EnteredSpan {
        span!(
            Level::DEBUG,
            "gc_collect",
            collection_type = collection_type,
            gc_id = gc_id.0
        )
        .entered()
    }

    /// Create a span for a GC phase (clear/mark/sweep).
    #[allow(dead_code)]
    pub fn trace_phase(phase: GcPhase) -> span::EnteredSpan {
        span!(Level::DEBUG, "gc_phase", phase = ?phase).entered()
    }

    /// Log the start of a GC phase.
    #[allow(dead_code)]
    pub fn log_phase_start(phase: GcPhase, bytes_before: usize) {
        tracing::debug!(phase = ?phase, bytes_before, "phase_start");
    }

    /// Log the end of a GC phase.
    #[allow(dead_code)]
    pub fn log_phase_end(phase: GcPhase, bytes_reclaimed: usize) {
        tracing::debug!(phase = ?phase, bytes_reclaimed, "phase_end");
    }
}

#[cfg(not(feature = "tracing"))]
pub mod internal {
    /// Stub type when tracing is disabled.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GcId(pub u64);

    /// Stub function when tracing is disabled.
    pub fn next_gc_id() -> GcId {
        GcId(0)
    }
}

// Re-export GcId at the module level for convenience
#[cfg(feature = "tracing")]
pub use internal::GcId;
#[cfg(not(feature = "tracing"))]
pub use internal::GcId;
