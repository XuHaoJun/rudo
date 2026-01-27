#![allow(dead_code)]
#![allow(clippy::arc_with_non_send_sync)]
#![allow(clippy::unused_self)]
#![allow(clippy::use_self)]

//! Parallel marking coordinator and worker implementations.
//!
//! This module provides the core infrastructure for parallel garbage collection marking,
//! including work distribution, synchronization, and coordination across multiple threads.
//!
//! # Lock Ordering
//!
//! `PerThreadMarkQueue` operations must follow the lock ordering discipline:
//! - `LocalHeap` (order 1) - Per-thread allocation
//! - `GlobalMarkState` (order 2) - Mark phase coordination
//! - `GC Request` (order 3) - GC trigger
//!
//! Workers should never hold `PerThreadMarkQueue` references while acquiring higher-order locks.

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Mutex;
use std::sync::MutexGuard;

use super::worklist::StealQueue;
use crate::heap::PageHeader;
use crate::ptr::GcBox;
use crate::trace::{GcVisitor, VisitorKind};

/// A per-thread mark queue that holds objects to be traced.
/// Objects are pushed to the local end (LIFO) for cache efficiency,
/// and can be stolen from the remote end (FIFO) by other threads.
///
/// # Push-Based Work Transfer
///
/// This queue supports push-based work transfer to reduce steal contention.
/// When a worker encounters a remote reference, it can push work directly
/// to the owner's `pending_work` queue. The owner checks pending work
/// before attempting to steal.
///
/// # Ownership Tracking
///
/// The queue tracks pages owned by this worker for ownership-based load
/// distribution. Workers prioritize marking their owned pages for cache locality.
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
    /// Pending work received from other threads (push-based transfer).
    /// Uses Mutex for safe concurrent access from multiple pushers.
    pending_work: Mutex<Vec<*const GcBox<()>>>,
}

const MARK_QUEUE_SIZE: usize = 1024;

/// Buffer size for pending work received via push-based transfer.
/// Fixed small buffer to minimize memory overhead (8-16 items).
const PENDING_WORK_BUFFER_SIZE: usize = 16;

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
            pending_work: Mutex::new(Vec::with_capacity(PENDING_WORK_BUFFER_SIZE)),
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
        // First check pending work from push-based transfer
        if let Some(obj) = self.receive_pending_work() {
            return Some(obj);
        }
        // Then try stealing
        for other in other_queues {
            if other.worker_idx() == self.worker_idx {
                continue;
            }
            if let Some(obj) = other.steal() {
                return Some(obj);
            }
        }
        None
    }

    // ============================================================================
    // Push-Based Work Transfer
    // ============================================================================

    /// Push work to another worker's pending queue.
    ///
    /// This is the push-based transfer: instead of all workers polling,
    /// when a worker encounters a remote reference, it pushes the work
    /// directly to the owner's pending queue.
    ///
    /// # Arguments
    ///
    /// * `owner` - The queue to push work to (owner's queue)
    /// * `work` - The work item to push
    ///
    /// # Lock Ordering
    ///
    /// Acquires `owner.pending_work` lock. This is a per-queue lock with
    /// order equivalent to `LocalHeap` (order 1).
    pub fn push_remote(owner: &Arc<PerThreadMarkQueue>, work: *const GcBox<()>) {
        let mut pending = owner.pending_work.lock().unwrap();
        pending.push(work);
        // Note: In a more sophisticated implementation, we would notify
        // the owner here. For simplicity, workers poll their pending_work
        // when their local queue is empty.
    }

    /// Receive all pending work from other workers.
    ///
    /// Called when the local queue is empty. Drains the pending work buffer
    /// and returns all items for processing.
    ///
    /// # Returns
    ///
    /// Some(work) if pending work exists, None otherwise
    ///
    /// # Lock Ordering
    ///
    /// Acquires `self.pending_work` lock. This is a per-queue lock with
    /// order equivalent to `LocalHeap` (order 1).
    pub fn receive_pending_work(&self) -> Option<*const GcBox<()>> {
        let mut pending = self.pending_work.lock().unwrap();
        pending.pop()
    }

    /// Drain all pending work at once.
    ///
    /// More efficient than repeated calls to `receive_pending_work()`.
    ///
    /// # Lock Ordering
    ///
    /// Acquires `self.pending_work` lock.
    pub fn drain_pending_work(&self) -> Vec<*const GcBox<()>> {
        let mut pending = self.pending_work.lock().unwrap();
        std::mem::take(&mut pending)
    }

    /// Wait for work to become available.
    ///
    /// Called when a worker has no work in either the local queue
    /// or pending work buffer. Waits for notification from a pusher.
    ///
    /// Note: This implementation uses a simple polling loop with exponential
    /// backoff to avoid blocking indefinitely. In a production system, this
    /// could be replaced with proper async/await or a condvar-based approach.
    ///
    /// # Lock Ordering
    ///
    /// Uses `self.pending_work` for condition synchronization.
    #[allow(dead_code)]
    pub fn wait_for_work(&self, timeout_ms: u64) -> bool {
        let mut backoff = 1;
        let max_backoff = 1024;
        let start = std::time::Instant::now();

        while start.elapsed().as_millis() < u128::from(timeout_ms) {
            if self.has_pending_work() || !self.queue.is_empty(&self.bottom) {
                return true;
            }
            // Exponential backoff to reduce CPU usage
            std::thread::sleep(std::time::Duration::from_millis(backoff));
            backoff = (backoff * 2).min(max_backoff);
        }
        false
    }

    /// Check if pending work is available.
    #[must_use]
    pub fn has_pending_work(&self) -> bool {
        let pending = self.pending_work.lock().unwrap();
        !pending.is_empty()
    }

    /// Get the number of pending work items.
    #[must_use]
    pub fn pending_work_len(&self) -> usize {
        let pending = self.pending_work.lock().unwrap();
        pending.len()
    }

    // ============================================================================
    // Ownership-Based Load Distribution
    // ============================================================================

    /// Try to steal from queues of page owners first.
    ///
    /// Prioritizes stealing from workers who own pages, improving cache locality
    /// by keeping work near the thread that allocated the data.
    ///
    /// # Arguments
    ///
    /// * `all_queues` - All worker queues
    /// * `page_owners` - Map of page pointers to owner worker indices
    ///
    /// # Returns
    ///
    /// Stolen work item if successful
    pub fn try_steal_owned_work(
        &self,
        all_queues: &[PerThreadMarkQueue],
    ) -> Option<*const GcBox<()>> {
        // First check pending work
        if let Some(obj) = self.receive_pending_work() {
            return Some(obj);
        }
        // Then try stealing from owners' queues
        for other in all_queues {
            if other.worker_idx() == self.worker_idx {
                continue;
            }
            // Skip if we don't own any of the same pages
            if !self.has_overlapping_ownership(other) {
                continue;
            }
            if let Some(obj) = other.steal() {
                return Some(obj);
            }
        }
        // Fall back to regular stealing - convert to references
        let queue_refs: Vec<&PerThreadMarkQueue> = all_queues.iter().collect();
        self.work_steal(&queue_refs)
    }

    /// Check if two workers have overlapping page ownership.
    fn has_overlapping_ownership(&self, other: &PerThreadMarkQueue) -> bool {
        // Simplified: assume overlap if either has owned pages
        // A more sophisticated implementation would track shared pages
        !self.owned_pages.is_empty() || !other.owned_pages.is_empty()
    }

    /// Get the number of owned pages.
    #[must_use]
    pub fn owned_page_count(&self) -> usize {
        self.owned_pages.len()
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
            pending_work: Mutex::new(Vec::with_capacity(PENDING_WORK_BUFFER_SIZE)),
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
