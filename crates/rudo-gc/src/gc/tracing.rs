//! GC-level tracing spans.

#[cfg(feature = "tracing")]
use tracing::Span;

/// Create a span for incremental marking.
#[cfg(feature = "tracing")]
pub fn span_incremental_mark(phase: &str) -> Span {
    tracing::debug_span!("incremental_mark", phase = phase)
}

/// Log an incremental mark slice completion event.
#[cfg(feature = "tracing")]
pub fn log_incremental_slice(objects_marked: usize, dirty_pages: usize) {
    tracing::debug!(
        objects_marked = objects_marked,
        dirty_pages = dirty_pages,
        "incremental_slice"
    );
}

/// Log a fallback to stop-the-world event.
#[cfg(feature = "tracing")]
pub fn log_fallback(reason: &str) {
    tracing::debug!(reason = reason, "fallback");
}

/// Log the start of incremental marking.
#[cfg(feature = "tracing")]
pub fn log_incremental_start(budget: usize, gc_id: crate::tracing::GcId) {
    tracing::debug!(budget = budget, gc_id = gc_id.0, "incremental_start");
}

/// Log a phase transition during incremental marking.
#[cfg(feature = "tracing")]
pub fn log_phase_transition(phase: &str, objects_marked: usize) {
    tracing::debug!(
        phase = phase,
        objects_marked = objects_marked,
        "phase_transition"
    );
}
