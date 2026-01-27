#![allow(dead_code)]
#![allow(clippy::arc_with_non_send_sync)]
#![allow(clippy::unused_self)]
#![allow(clippy::use_self)]

//! Parallel marking coordinator and worker implementations.
//!
//! This module provides the core infrastructure for parallel garbage collection marking,
//! including work distribution, synchronization, and coordination across multiple threads.

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Barrier;

use super::worklist::StealQueue;
use crate::heap::PageHeader;
use crate::ptr::GcBox;
use crate::trace::{GcVisitor, VisitorKind};

/// A per-thread mark queue that holds objects to be traced.
/// Objects are pushed to the local end (LIFO) for cache efficiency,
/// and can be stolen from the remote end (FIFO) by other threads.
pub struct PerThreadMarkQueue {
    /// The work-stealing queue for this thread's mark work.
    /// Uses usize to ensure Send + Sync (raw pointers aren't automatically Send).
    #[allow(clippy::arc_with_non_send_sync)]
    queue: Arc<StealQueue<usize, MARK_QUEUE_SIZE>>,
    /// The bottom index for local push/pop operations.
    bottom: Cell<usize>,
    /// Worker index for this queue.
    worker_idx: usize,
    /// Pages owned by this worker for processing.
    owned_pages: Vec<NonNull<PageHeader>>,
    /// Count of objects marked by this worker.
    marked_count: AtomicUsize,
}

const MARK_QUEUE_SIZE: usize = 1024;

impl PerThreadMarkQueue {
    /// Create a new per-thread mark queue with the given worker index.
    #[must_use]
    pub fn new_with_index(worker_idx: usize) -> Self {
        Self {
            queue: Arc::new(StealQueue::new()),
            bottom: Cell::new(0),
            worker_idx,
            owned_pages: Vec::new(),
            marked_count: AtomicUsize::new(0),
        }
    }

    /// Create a new per-thread mark queue.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_index(0)
    }

    /// Push an object onto this thread's mark queue.
    /// Returns true if successful, false if the queue is full.
    pub fn push(&self, obj: *const GcBox<()>) -> bool {
        self.queue.push(&self.bottom, obj as usize)
    }

    /// Push from a `NonNull` pointer.
    pub fn push_non_null(&self, obj: NonNull<GcBox<()>>) -> bool {
        self.push(obj.as_ptr())
    }

    /// Pop an object from the local end (LIFO).
    /// Returns None if the queue is empty.
    pub fn pop(&self) -> Option<*const GcBox<()>> {
        self.queue
            .pop(&self.bottom)
            .map(|ptr| ptr as *const GcBox<()>)
    }

    /// Steal an object from the remote end (FIFO).
    /// Called by other threads to steal work.
    /// Returns None if the queue is empty.
    pub fn steal(&self) -> Option<*const GcBox<()>> {
        self.queue
            .steal(&self.bottom)
            .map(|ptr| ptr as *const GcBox<()>)
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

    /// Try to steal work from another queue.
    /// Returns the stolen item if successful, None otherwise.
    #[must_use]
    pub fn try_steal_from(&self, other: &PerThreadMarkQueue) -> Option<*const GcBox<()>> {
        other.steal()
    }

    /// Work stealing algorithm: attempt to steal from other queues
    /// when local queue is empty or nearly empty.
    /// Iterates through other queues in FIFO order to find work.
    #[allow(dead_code)]
    pub fn work_steal(&self, other_queues: &[&PerThreadMarkQueue]) -> Option<*const GcBox<()>> {
        for other in other_queues {
            if let Some(obj) = other.steal() {
                return Some(obj);
            }
        }
        None
    }

    /// Get a reference to the underlying queue for sharing with other threads.
    #[must_use]
    pub const fn queue(&self) -> &Arc<StealQueue<usize, MARK_QUEUE_SIZE>> {
        &self.queue
    }

    /// Get the worker index for this queue.
    #[must_use]
    pub const fn worker_idx(&self) -> usize {
        self.worker_idx
    }

    /// Register a page as owned by this worker.
    pub fn add_owned_page(&mut self, page: NonNull<PageHeader>) {
        self.owned_pages.push(page);
    }

    /// Get the number of objects marked by this worker.
    #[must_use]
    pub fn marked_count(&self) -> usize {
        self.marked_count.load(Ordering::Relaxed)
    }

    /// Process all objects on an owned page.
    /// Returns the number of objects marked on this page.
    unsafe fn process_owned_page(&self, page: NonNull<PageHeader>, kind: VisitorKind) -> usize {
        let header = page.as_ptr();
        let mut marked = 0;
        let block_size = unsafe { (*header).block_size } as usize;
        let header_size = PageHeader::header_size(block_size);
        let obj_count = unsafe { (*header).obj_count } as usize;

        for i in 0..obj_count {
            if unsafe { (*header).is_allocated(i) && !(*header).is_marked(i) } {
                if kind == VisitorKind::Minor && unsafe { (*header).generation } > 0 {
                    continue;
                }

                let obj_ptr = unsafe { page.cast::<u8>().add(header_size + i * block_size) };
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                unsafe { (*header).set_mark(i) };
                marked += 1;

                self.push(gc_box_ptr.as_ptr());
            }
        }

        marked
    }

    /// Increment the marked count.
    pub fn inc_marked_count(&self, count: usize) {
        self.marked_count.fetch_add(count, Ordering::Relaxed);
    }
}

impl Default for PerThreadMarkQueue {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl Send for PerThreadMarkQueue {}

unsafe impl Sync for PerThreadMarkQueue {}

/// Configuration for parallel marking behavior.
#[derive(Clone, Copy, Debug)]
pub struct ParallelMarkConfig {
    /// Maximum number of worker threads for parallel marking.
    /// If 0 or 1, single-threaded fallback is used.
    pub max_workers: usize,
    /// Capacity of each per-thread mark queue.
    pub queue_capacity: usize,
    /// Whether to enable parallel Major GC marking.
    pub parallel_major_gc: bool,
    /// Whether to enable parallel Minor GC marking.
    pub parallel_minor_gc: bool,
    /// Number of steal attempts before yielding.
    pub steal_attempts_before_yield: u32,
    /// Whether to enable work stealing load balancing.
    pub work_stealing_enabled: bool,
}

impl Default for ParallelMarkConfig {
    fn default() -> Self {
        Self {
            max_workers: 4,
            queue_capacity: 1024,
            parallel_major_gc: true,
            parallel_minor_gc: true,
            steal_attempts_before_yield: 10,
            work_stealing_enabled: true,
        }
    }
}

impl ParallelMarkConfig {
    /// Create a new configuration with the given maximum worker count.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers: max_workers.max(1),
            ..Default::default()
        }
    }

    /// Get the actual number of workers to use.
    /// Returns 1 if fewer than 2 workers requested (single-threaded fallback).
    #[must_use]
    pub fn effective_workers(&self) -> usize {
        self.max_workers.max(1)
    }

    /// Check if parallel marking should be used.
    #[must_use]
    pub fn use_parallel(&self) -> bool {
        self.effective_workers() > 1
    }

    /// Set the maximum number of worker threads.
    pub const fn set_max_workers(&mut self, workers: usize) {
        self.max_workers = if workers < 1 { 1 } else { workers };
    }

    /// Enable or disable parallel Major GC.
    pub const fn set_parallel_major_gc(&mut self, enabled: bool) {
        self.parallel_major_gc = enabled;
    }

    /// Enable or disable parallel Minor GC.
    pub const fn set_parallel_minor_gc(&mut self, enabled: bool) {
        self.parallel_minor_gc = enabled;
    }

    /// Enable or disable work stealing.
    pub const fn set_work_stealing(&mut self, enabled: bool) {
        self.work_stealing_enabled = enabled;
    }
}

/// Coordinates parallel marking across multiple worker threads.
///
/// The coordinator manages the parallel marking phase of garbage collection.
/// It uses atomic operations for coordination, minimizing the need for locks.
/// Any locks used by workers must follow the lock ordering discipline:
///
/// 1. `LocalHeap` (order 1) - Per-thread allocation
/// 2. `GlobalMarkState` (order 2) - Mark phase coordination
/// 3. `GC Request` (order 3) - GC trigger
///
/// Workers must not acquire any locks while holding `PerThreadMarkQueue` references.
pub struct ParallelMarkCoordinator {
    /// The number of worker threads participating in marking.
    num_workers: usize,
    /// Barrier for synchronizing workers at the end of marking.
    barrier: Arc<Barrier>,
    /// Shared counter for pages that need processing.
    pages_remaining: AtomicUsize,
    /// Flag indicating marking is complete.
    marking_complete: AtomicUsize,
    /// Total marked count.
    total_marked: AtomicUsize,
}

impl ParallelMarkCoordinator {
    /// Create a new coordinator for the given number of workers.
    #[must_use]
    pub fn new(num_workers: usize) -> Self {
        Self {
            num_workers: num_workers.max(1),
            barrier: Arc::new(Barrier::new(num_workers.max(1))),
            pages_remaining: AtomicUsize::new(0),
            marking_complete: AtomicUsize::new(0),
            total_marked: AtomicUsize::new(0),
        }
    }

    /// Create a new coordinator with configuration.
    /// Uses single-threaded fallback if fewer than 2 workers requested.
    #[must_use]
    pub fn with_config(config: &ParallelMarkConfig) -> Self {
        Self::new(config.effective_workers())
    }

    /// Get the effective number of workers (at least 1).
    #[must_use]
    pub const fn num_workers(&self) -> usize {
        self.num_workers
    }

    /// Check if running in single-threaded mode.
    #[must_use]
    pub const fn is_single_threaded(&self) -> bool {
        self.num_workers <= 1
    }

    /// Check if parallel marking should be used based on configuration and worker count.
    #[must_use]
    pub fn should_use_parallel(&self, config: &ParallelMarkConfig) -> bool {
        config.use_parallel() && self.num_workers > 1
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

    /// Distribute dirty pages to worker queues for Minor GC parallel marking.
    ///
    /// Dirty pages are pages that have been written to since the last GC.
    /// During Minor GC, we need to scan these pages for old->young references.
    /// This function distributes dirty pages evenly across worker queues.
    #[allow(clippy::similar_names)]
    pub fn distribute_dirty_pages(
        &self,
        dirty_pages: &[*const PageHeader],
        worker_queues: &[PerThreadMarkQueue],
    ) -> Vec<usize> {
        let num_workers = worker_queues.len().max(1);
        let mut distribution = Vec::with_capacity(dirty_pages.len());

        for (idx, _page) in dirty_pages.iter().enumerate() {
            let worker_idx = idx % num_workers;
            distribution.push(worker_idx);
        }

        distribution
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

    /// Get the total number of objects marked.
    #[must_use]
    pub fn total_marked(&self) -> usize {
        self.total_marked.load(Ordering::Acquire)
    }

    /// Record marked objects from a worker.
    pub fn record_marked(&self, count: usize) {
        self.total_marked.fetch_add(count, Ordering::Relaxed);
    }

    /// Wait at the barrier for all workers to synchronize.
    pub fn wait_at_barrier(&self) {
        self.barrier.wait();
    }

    /// Mark that this worker has completed.
    pub fn worker_completed(&self) {
        self.marking_complete.fetch_add(1, Ordering::Release);
    }
}

/// Execute the mark phase of garbage collection on a work queue.
/// This function is run by each worker thread.
///
/// Algorithm (from spec section 4.1):
/// 1. Process owned pages
/// 2. Process local queue (LIFO)
/// 3. Steal from other queues (FIFO) when local queue empty
pub fn worker_mark_loop(
    queue: &PerThreadMarkQueue,
    all_queues: &[PerThreadMarkQueue],
    kind: VisitorKind,
) -> usize {
    let mut marked = 0;
    let mut visitor = GcVisitor::new(kind);

    loop {
        while let Some(obj) = queue.pop() {
            unsafe {
                let ptr_addr = obj.cast::<GcBox<()>>().cast::<u8>();
                let header = crate::heap::ptr_to_page_header(ptr_addr);

                if header.as_ref().magic != crate::heap::MAGIC_GC_PAGE {
                    continue;
                }

                let Some(idx) = crate::heap::ptr_to_object_index(obj.cast()) else {
                    continue;
                };

                if (*header.as_ptr()).is_marked(idx) {
                    continue;
                }

                (*header.as_ptr()).set_mark(idx);
                marked += 1;

                let gc_box_ptr = obj.cast_mut();
                ((*gc_box_ptr).trace_fn)(ptr_addr, &mut visitor);
            }
        }

        if !try_steal_work(queue, all_queues) {
            break;
        }
    }

    marked
}

/// Try to steal work from other queues.
/// Returns true if work was stolen, false if all queues are empty.
fn try_steal_work(queue: &PerThreadMarkQueue, all_queues: &[PerThreadMarkQueue]) -> bool {
    for other in all_queues {
        if other.worker_idx() == queue.worker_idx() {
            continue;
        }

        if let Some(obj) = other.steal() {
            if queue.push(obj) {
                return true;
            }
            for other2 in all_queues {
                if other2.worker_idx() == queue.worker_idx() {
                    continue;
                }
                if other2.push(obj) {
                    return true;
                }
            }
        }
    }

    false
}

impl Clone for PerThreadMarkQueue {
    fn clone(&self) -> Self {
        Self {
            queue: Arc::clone(&self.queue),
            bottom: Cell::new(self.bottom.get()),
            worker_idx: self.worker_idx,
            owned_pages: Vec::new(),
            marked_count: AtomicUsize::new(0),
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
    (0..count).map(PerThreadMarkQueue::new_with_index).collect()
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
