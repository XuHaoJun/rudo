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

pub mod mark;
pub mod marker;
pub mod sync;
pub mod worklist;

// Re-exports from gc
pub use gc::{
    clear_test_roots, collect, collect_full, default_collect_condition, is_collecting, mark_object,
    mark_object_minor, notify_created_gc, notify_dropped_gc, register_test_root, safepoint,
    set_collect_condition, CollectInfo,
};

#[cfg(feature = "lazy-sweep")]
pub use gc::{pending_sweep_count, sweep_pending, sweep_specific_page};

// Re-exports from marker
pub use marker::{
    worker_mark_loop_with_registry, GcWorkerRegistry, ParallelMarkConfig, PerThreadMarkQueue,
};

// Re-exports from worklist
pub use worklist::StealQueue;
