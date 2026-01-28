#![allow(unused)]

//! Garbage collection coordination and parallel marking.
//!
//! This module provides the core garbage collection infrastructure including:
//! - Parallel marking coordinator and worker implementations
//! - Work-stealing queue for load balancing
//! - Lock ordering discipline for deadlock prevention
//! - Mark phase optimizations (bitmap, ownership, push-based transfer)

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

// Re-exports from marker
pub use marker::{ParallelMarkConfig, ParallelMarkCoordinator, PerThreadMarkQueue};

// Re-exports from worklist
pub use worklist::StealQueue;
