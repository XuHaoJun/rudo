#![allow(unused)]

//! Garbage collection coordination and parallel marking.
//!
//! This module provides the core garbage collection infrastructure including:
//! - Parallel marking coordinator and worker implementations
//! - Work-stealing queue for load balancing
//! - Lock ordering discipline for deadlock prevention
//! - Mark phase optimizations (bitmap, ownership, push-based transfer)

#[allow(clippy::module_inception)]
mod gc;

pub mod incremental;
pub mod mark;
pub mod marker;
pub mod sync;
pub mod worklist;

#[cfg(feature = "debug-suspicious-sweep")]
#[allow(missing_docs)]
pub mod young_object_history;

#[cfg(feature = "debug-suspicious-sweep")]
pub use young_object_history::{
    clear_history, current_gc_cycle_id, get_gc_cycle_id, is_detection_enabled, is_suspicious_sweep,
    record_young_object, set_detection_enabled,
};

#[cfg(feature = "tracing")]
pub mod tracing;

// Re-exports from gc
pub use gc::{
    clear_test_roots, collect, collect_full, default_collect_condition, is_collecting, mark_object,
    mark_object_minor, notify_created_gc, notify_dropped_gc, register_test_root,
    register_test_root_region, safepoint, set_collect_condition, CollectInfo,
};

#[cfg(any(test, feature = "test-util"))]
pub use gc::iter_test_roots;

#[cfg(feature = "lazy-sweep")]
pub use gc::{pending_sweep_count, sweep_pending, sweep_specific_page};

// Re-exports from marker
pub use marker::{
    worker_mark_loop_with_registry, GcWorkerRegistry, ParallelMarkConfig, PerThreadMarkQueue,
};

// Re-exports from worklist
pub use worklist::StealQueue;
