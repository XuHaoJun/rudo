#![allow(dead_code)]
#![allow(clippy::arc_with_non_send_sync)]

//! Parallel marking coordinator and worker implementations.
//!
//! This module provides the core infrastructure for parallel garbage collection marking,
//! including work distribution, synchronization, and coordination across multiple threads.

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::worklist::StealQueue;
use crate::heap::PageHeader;
use crate::ptr::GcBox;
use crate::trace::{Trace, Visitor};
use crate::Gc;

/// A visitor that pushes discovered objects to a mark queue.
struct MarkQueueVisitor<'a> {
    queue: &'a PerThreadMarkQueue,
}

impl<'a> MarkQueueVisitor<'a> {
    #[must_use]
    const fn new(queue: &'a PerThreadMarkQueue) -> Self {
        Self { queue }
    }
}

impl Visitor for MarkQueueVisitor<'_> {
    fn visit<T: Trace>(&mut self, gc: &Gc<T>) {
        #[allow(clippy::ptr_as_ptr)]
        let ptr = Gc::<T>::as_ptr(gc) as *const GcBox<()>;
        self.queue.push(ptr);
    }

    unsafe fn visit_region(&mut self, _ptr: *const u8, _len: usize) {
        // Conservative scanning would go here for native stacks/globals
    }
}

/// A per-thread mark queue that holds objects to be traced.
/// Objects are pushed to the local end (LIFO) for cache efficiency,
/// and can be stolen from the remote end (FIFO) by other threads.
pub struct PerThreadMarkQueue {
    /// The work-stealing queue for this thread's mark work.
    #[allow(clippy::arc_with_non_send_sync)]
    queue: Arc<StealQueue<*const GcBox<()>, MARK_QUEUE_SIZE>>,
    /// The bottom index for local push/pop operations.
    bottom: Cell<usize>,
}

const MARK_QUEUE_SIZE: usize = 1024;

impl PerThreadMarkQueue {
    /// Create a new per-thread mark queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queue: Arc::new(StealQueue::new()),
            bottom: Cell::new(0),
        }
    }

    /// Push an object onto this thread's mark queue.
    /// Returns true if successful, false if the queue is full.
    pub fn push(&self, obj: *const GcBox<()>) -> bool {
        self.queue.push(&self.bottom, obj)
    }

    /// Pop an object from the local end (LIFO).
    /// Returns None if the queue is empty.
    pub fn pop(&self) -> Option<*const GcBox<()>> {
        self.queue.pop(&self.bottom)
    }

    /// Steal an object from the remote end (FIFO).
    /// Called by other threads to steal work.
    /// Returns None if the queue is empty.
    pub fn steal(&self) -> Option<*const GcBox<()>> {
        self.queue.steal(&self.bottom)
    }

    /// Get the current length of the queue.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len(&self.bottom)
    }

    /// Check if the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty(&self.bottom)
    }

    /// Get a reference to the underlying queue for sharing with other threads.
    #[must_use]
    pub const fn queue(&self) -> &Arc<StealQueue<*const GcBox<()>, MARK_QUEUE_SIZE>> {
        &self.queue
    }
}

impl Default for PerThreadMarkQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Coordinates parallel marking across multiple worker threads.
pub struct ParallelMarkCoordinator {
    /// The number of worker threads participating in marking.
    num_workers: usize,
    /// Barrier for synchronizing workers at the end of marking.
    barrier: Arc<AtomicUsize>,
    /// Shared counter for pages that need processing.
    pages_remaining: AtomicUsize,
    /// Flag indicating marking is complete.
    marking_complete: AtomicUsize,
}

impl ParallelMarkCoordinator {
    /// Create a new coordinator for the given number of workers.
    #[must_use]
    pub fn new(num_workers: usize) -> Self {
        Self {
            num_workers,
            barrier: Arc::new(AtomicUsize::new(0)),
            pages_remaining: AtomicUsize::new(0),
            marking_complete: AtomicUsize::new(0),
        }
    }

    /// Start parallel marking with the given worker queues.
    pub fn start_marking(
        &self,
        _worker_queues: &[PerThreadMarkQueue],
        root_pages: &[*const PageHeader],
    ) {
        self.pages_remaining
            .store(root_pages.len(), Ordering::Release);
        self.marking_complete.store(0, Ordering::Release);
    }

    /// Wait for all workers to complete marking.
    pub fn wait_for_completion(&self) {
        while self.marking_complete.load(Ordering::Acquire) < self.num_workers {
            std::hint::spin_loop();
        }
    }

    /// Check if marking is complete.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.marking_complete.load(Ordering::Acquire) >= self.num_workers
    }

    /// Get the number of pages remaining to be processed.
    #[must_use]
    pub fn pages_remaining(&self) -> usize {
        self.pages_remaining.load(Ordering::Acquire)
    }
}

/// Execute the mark phase of garbage collection on a work queue.
/// This function is run by each worker thread.
///
/// NOTE: This is a placeholder for the actual implementation.
/// The current GC uses `GcVisitor` with its own worklist. For parallel marking,
/// we'll need to either:
/// 1. Modify `GcVisitor` to use our mark queue, or
/// 2. Create a custom tracing approach for parallel marking
#[allow(dead_code)]
const fn worker_mark_loop(_queue: &PerThreadMarkQueue, _coordinator: &ParallelMarkCoordinator) {
    // Implementation deferred until integration with gc.rs
}

/// Configuration for parallel marking.
#[derive(Debug, Clone, Copy)]
pub struct ParallelMarkConfig {
    /// Number of worker threads to use for parallel marking.
    /// If 0 or 1, single-threaded marking is used.
    pub num_workers: usize,
    /// Capacity of each worker's mark queue.
    pub queue_capacity: usize,
    /// Whether to enable parallel major GC marking.
    pub parallel_major_gc: bool,
    /// Whether to enable parallel minor GC marking.
    pub parallel_minor_gc: bool,
}

impl Default for ParallelMarkConfig {
    fn default() -> Self {
        Self {
            num_workers: 4,
            queue_capacity: MARK_QUEUE_SIZE,
            parallel_major_gc: true,
            parallel_minor_gc: true,
        }
    }
}

/// Get the number of CPUs available for parallel marking.
#[must_use]
pub fn available_parallelism() -> usize {
    std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

/// Create worker queues for parallel marking.
#[must_use]
pub fn create_worker_queues(count: usize) -> Vec<PerThreadMarkQueue> {
    (0..count).map(|_| PerThreadMarkQueue::new()).collect()
}

/// Initialize parallel marking infrastructure.
/// Returns the coordinator and worker queues.
#[must_use]
pub fn init_parallel_marking(
    num_workers: usize,
) -> (ParallelMarkCoordinator, Vec<PerThreadMarkQueue>) {
    let coordinator = ParallelMarkCoordinator::new(num_workers);
    let worker_queues = create_worker_queues(num_workers);
    (coordinator, worker_queues)
}
