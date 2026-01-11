//! `BiBOP` (Big Bag of Pages) memory management.
//!
//! This module implements the core memory layout using page-aligned segments
//! with size-class based allocation for O(1) allocation performance.
//!
//! # `BiBOP` Memory Layout
//!
//! Memory is divided into 4KB pages. Each page contains objects of a single
//! size class. This allows O(1) lookup of object metadata from its address.

use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

use sys_alloc::{Mmap, MmapOptions};

use crate::trace::{GcVisitor, VisitorKind};

// ============================================================================
// Thread Registry & Control Block - Multi-threaded GC Support
// ============================================================================

/// Thread state: executing mutator code.
pub const THREAD_STATE_EXECUTING: usize = 0;
/// Thread state: at a safe point, waiting for GC.
pub const THREAD_STATE_SAFEPOINT: usize = 1;
/// Thread state: inactive (blocked in syscall).
pub const THREAD_STATE_INACTIVE: usize = 2;

#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
pub const SWEEPER_NONE: usize = 0;
#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
pub const SWEEPER_READY: usize = 1;
#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
pub const SWEEPER_SWEEPING: usize = 2;
#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
pub const SWEEPER_WAITING_FOR_WORK: usize = 3;

#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
pub struct SweepWorker {
    pub id: usize,
    pub status: AtomicUsize,
    pub work_cond: Condvar,
    pub done_cond: Condvar,
    pub mutex: Mutex<()>,
    pub tcbs: Mutex<Vec<std::sync::Arc<ThreadControlBlock>>>,
}

/// Shared control block for each thread's GC coordination.
pub struct ThreadControlBlock {
    /// The current state of the thread.
    pub state: AtomicUsize,
    /// Whether a GC has been requested for this thread.
    pub gc_requested: AtomicBool,
    /// Condvar for parking the thread during GC.
    pub park_cond: Condvar,
    /// Mutex for parking the thread during GC.
    pub park_mutex: Mutex<()>,
    /// The thread's local heap.
    pub heap: UnsafeCell<LocalHeap>,
    /// Captured stack roots for the current GC cycle.
    pub stack_roots: Mutex<Vec<*const u8>>,
    /// The unique ID of this thread.
    pub thread_id: usize,
    /// The local mark stack for parallel marking.
    pub local_mark_stack: Mutex<Vec<*const u8>>,
    /// The remote inbox for objects forwarded from other threads.
    pub remote_inbox: Mutex<Vec<*const u8>>,
    /// Number of objects this thread has forwarded to other threads.
    pub remote_sent_count: AtomicUsize,
    /// Number of objects other threads have forwarded to this thread.
    pub remote_received_count: AtomicUsize,
    /// Buffer for batching remote forward operations.
    #[cfg(feature = "parallel-gc")]
    pub send_buffer: UnsafeCell<Vec<(*const u8, usize)>>,
}

#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for ThreadControlBlock {}
unsafe impl Sync for ThreadControlBlock {}

impl Default for ThreadControlBlock {
    fn default() -> Self {
        Self::new(0)
    }
}

impl ThreadControlBlock {
    /// Create a new `ThreadControlBlock` with an uninitialized heap.
    /// The heap must be initialized separately.
    #[must_use]
    pub fn new(thread_id: usize) -> Self {
        Self {
            state: AtomicUsize::new(THREAD_STATE_EXECUTING),
            gc_requested: AtomicBool::new(false),
            park_cond: Condvar::new(),
            park_mutex: Mutex::new(()),
            heap: UnsafeCell::new(LocalHeap::new()),
            stack_roots: Mutex::new(Vec::new()),
            thread_id,
            local_mark_stack: Mutex::new(Vec::new()),
            remote_inbox: Mutex::new(Vec::new()),
            remote_sent_count: AtomicUsize::new(0),
            remote_received_count: AtomicUsize::new(0),
            #[cfg(feature = "parallel-gc")]
            send_buffer: UnsafeCell::new(Vec::new()),
        }
    }

    /// Get the thread ID.
    #[must_use]
    pub const fn id(&self) -> usize {
        self.thread_id
    }

    /// Push an object to the local mark stack.
    ///
    /// # Panics
    ///
    /// Panics if the `local_mark_stack` lock is poisoned.
    pub fn push_local_mark(&self, ptr: *const u8) {
        let mut stack = self.local_mark_stack.lock().unwrap();
        stack.push(ptr);
    }

    /// Pop an object from the local mark stack.
    ///
    /// # Panics
    ///
    /// Panics if the `local_mark_stack` lock is poisoned.
    pub fn pop_local_mark(&self) -> Option<*const u8> {
        let mut stack = self.local_mark_stack.lock().unwrap();
        stack.pop()
    }

    /// Check if local mark stack is empty.
    ///
    /// # Panics
    ///
    /// Panics if the `local_mark_stack` lock is poisoned.
    pub fn is_local_mark_stack_empty(&self) -> bool {
        let stack = self.local_mark_stack.lock().unwrap();
        stack.is_empty()
    }

    /// Push an object to the remote inbox (for forwarding from other threads).
    ///
    /// # Panics
    ///
    /// Panics if the `remote_inbox` lock is poisoned.
    pub fn push_remote_inbox(&self, ptr: *const u8) {
        self.remote_inbox.lock().unwrap().push(ptr);
        self.remote_received_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Drain all objects from the remote inbox into the local mark stack.
    ///
    /// # Panics
    ///
    /// Panics if the `remote_inbox` or `local_mark_stack` lock is poisoned.
    pub fn drain_remote_inbox_to_local(&self) -> usize {
        let mut inbox = self.remote_inbox.lock().unwrap();
        let mut stack = self.local_mark_stack.lock().unwrap();
        let count = inbox.len();
        if count > 0 {
            stack.extend(inbox.drain(..));
        }
        count
    }

    /// Clear both local mark stack and remote inbox (after GC).
    ///
    /// # Panics
    ///
    /// Panics if the `remote_inbox` or `local_mark_stack` lock is poisoned.
    pub fn clear_mark_stacks(&self) {
        self.local_mark_stack.lock().unwrap().clear();
        self.remote_inbox.lock().unwrap().clear();
        self.remote_sent_count.store(0, Ordering::Relaxed);
        self.remote_received_count.store(0, Ordering::Relaxed);
    }

    /// Get the total number of remote operations (sent + received).
    pub fn remote_operation_count(&self) -> usize {
        self.remote_sent_count.load(Ordering::Relaxed)
            + self.remote_received_count.load(Ordering::Relaxed)
    }

    /// Increment the remote sent counter.
    pub fn record_remote_sent(&self) {
        self.remote_sent_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a mutable reference to the heap.
    pub fn heap_mut(&mut self) -> &mut LocalHeap {
        unsafe { &mut *self.heap.get() }
    }

    /// Get an immutable reference to the heap.
    pub fn heap(&self) -> &LocalHeap {
        unsafe { &*self.heap.get() }
    }

    /// Flush the batch buffer of remote forward operations.
    ///
    /// # Panics
    ///
    /// Panics if the `SWEEP_MUTEX` or `THREAD_REGISTRY` lock is poisoned.
    #[cfg(feature = "parallel-gc")]
    pub fn flush_send_buffer(&self) {
        let _guard = SWEEP_MUTEX.lock().unwrap();
        let buffer = unsafe { &mut *self.send_buffer.get() };
        if buffer.is_empty() {
            return;
        }
        let registry = thread_registry().lock().unwrap();
        for (ptr, target_worker_id) in buffer.drain(..) {
            if target_worker_id < registry.sweep_workers.len() {
                let target_worker = &registry.sweep_workers[target_worker_id];
                {
                    let tcbs = target_worker.tcbs.lock().unwrap();
                    if !tcbs.is_empty() {
                        let target_tcb = tcbs[0].clone();
                        drop(tcbs);
                        target_tcb.push_remote_inbox(ptr);
                        let status = target_worker.status.load(Ordering::Acquire);
                        if status == SWEEPER_WAITING_FOR_WORK {
                            target_worker
                                .status
                                .store(SWEEPER_SWEEPING, Ordering::Release);
                            NUM_RUNNING_SWEEPERS.fetch_add(1, Ordering::SeqCst);
                            target_worker.work_cond.notify_one();
                        }
                    }
                }
            }
        }
        drop(registry);
    }

    /// Queue a remote forward operation in the batch buffer.
    ///
    /// # Panics
    ///
    /// Panics if the `SWEEP_MUTEX` or `THREAD_REGISTRY` lock is poisoned.
    #[cfg(feature = "parallel-gc")]
    pub fn queue_remote_forward(&self, ptr: *const u8, target_worker_id: usize) {
        let buffer = unsafe { &mut *self.send_buffer.get() };
        buffer.push((ptr, target_worker_id));
        if buffer.len() >= BATCH_FORWARD_SIZE {
            self.flush_send_buffer();
        }
    }
}

/// Global registry of all threads with GC heaps.
pub struct ThreadRegistry {
    /// All active thread control blocks.
    pub threads: Vec<std::sync::Arc<ThreadControlBlock>>,
    /// Number of threads currently in EXECUTING state.
    pub active_count: AtomicUsize,
    /// Global flag indicating if a GC collection is currently in progress.
    pub gc_in_progress: AtomicBool,
    /// Pool of worker threads for parallel GC.
    #[cfg(feature = "parallel-gc")]
    pub sweep_workers: Vec<std::sync::Arc<SweepWorker>>,
    /// Number of parallel workers currently active.
    #[cfg(feature = "parallel-gc")]
    pub num_running_sweepers: AtomicUsize,
    /// Target number of parallel worker threads.
    #[cfg(feature = "parallel-gc")]
    pub worker_count: usize,
}

impl Clone for ThreadRegistry {
    fn clone(&self) -> Self {
        Self {
            threads: self.threads.clone(),
            active_count: AtomicUsize::new(self.active_count.load(Ordering::Relaxed)),
            gc_in_progress: AtomicBool::new(self.gc_in_progress.load(Ordering::Relaxed)),
            #[cfg(feature = "parallel-gc")]
            sweep_workers: Vec::new(),
            #[cfg(feature = "parallel-gc")]
            num_running_sweepers: AtomicUsize::new(0),
            #[cfg(feature = "parallel-gc")]
            worker_count: 0,
        }
    }
}

impl Default for ThreadRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadRegistry {
    /// Create a new empty thread registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            threads: Vec::new(),
            active_count: AtomicUsize::new(0),
            gc_in_progress: AtomicBool::new(false),
            #[cfg(feature = "parallel-gc")]
            sweep_workers: Vec::new(),
            #[cfg(feature = "parallel-gc")]
            num_running_sweepers: AtomicUsize::new(0),
            #[cfg(feature = "parallel-gc")]
            worker_count: 0,
        }
    }

    /// Register a new thread with the registry.
    pub fn register_thread(&mut self, tcb: std::sync::Arc<ThreadControlBlock>) {
        self.threads.push(tcb);
        self.active_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Unregister a thread from the registry.
    pub fn unregister_thread(&mut self, tcb: &std::sync::Arc<ThreadControlBlock>) {
        self.threads
            .retain(|existing| !std::sync::Arc::ptr_eq(existing, tcb));
    }

    /// Mark that a GC collection is in progress.
    /// This is used to detect if GC is in progress when new threads spawn,
    /// since thread-local flags can't be shared across threads.
    pub fn set_gc_in_progress(&self, in_progress: bool) {
        self.gc_in_progress.store(in_progress, Ordering::SeqCst);
    }

    /// Check if a GC collection is currently in progress.
    /// This uses a global flag instead of thread-local, so it works
    /// correctly when called from newly spawned threads.
    #[must_use]
    pub fn is_gc_in_progress(&self) -> bool {
        self.gc_in_progress.load(Ordering::Acquire)
    }

    /// Ensure that the parallel worker pool is initialized.
    ///
    /// # Panics
    ///
    /// Panics if the `ALL_WORKERS` lock is poisoned.
    #[cfg(feature = "parallel-gc")]
    pub fn ensure_worker_pool_initialized(&mut self) {
        if self.sweep_workers.is_empty() {
            self.worker_count = get_worker_count();
            initialize_worker_pool();
        }
    }

    /// Distribute thread control blocks among the parallel workers.
    ///
    /// # Panics
    ///
    /// Panics if any of the workers' `tcbs` locks are poisoned.
    #[cfg(feature = "parallel-gc")]
    pub fn distribute_tcbs_to_workers(&mut self) {
        let worker_count = self.sweep_workers.len();
        if worker_count == 0 {
            return;
        }

        for worker in &self.sweep_workers {
            worker.tcbs.lock().unwrap().clear();
        }

        for (idx, tcb) in self.threads.iter().enumerate() {
            let worker_idx = idx % worker_count;
            if worker_idx < self.sweep_workers.len() {
                self.sweep_workers[worker_idx]
                    .tcbs
                    .lock()
                    .unwrap()
                    .push(tcb.clone());
            }
        }
    }
}

static THREAD_REGISTRY: OnceLock<Mutex<ThreadRegistry>> = OnceLock::new();

/// Access the global thread registry.
pub fn thread_registry() -> &'static Mutex<ThreadRegistry> {
    THREAD_REGISTRY.get_or_init(|| Mutex::new(ThreadRegistry::new()))
}

// ============================================================================
// Safe Points - Multi-threaded GC Coordination
// ============================================================================

/// Global flag set by collector to request all threads to stop at safe point.
/// Uses Relaxed ordering for fast-path reads - synchronization happens via the
/// rendezvous protocol, not this flag alone.
pub static GC_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Check if GC has been requested and handle the rendezvous if so.
/// This is the fast-path check inserted into allocation code.
pub fn check_safepoint() {
    // CRITICAL FIX: Prevent deadlock when Drop handlers allocate during GC
    // If we're already collecting, we must NOT enter rendezvous or we'll
    // deadlock waiting for gc_requested to become false (only collector can clear it)
    if GC_REQUESTED.load(Ordering::Relaxed) && !crate::gc::is_collecting() {
        enter_rendezvous();
    }
}

/// Called when a thread reaches a safe point and GC is requested.
/// Performs the cooperative rendezvous protocol.
#[allow(clippy::significant_drop_tightening)]
fn enter_rendezvous() {
    let Some(tcb) = current_thread_control_block() else {
        return;
    };

    // CRITICAL FIX: Check per-thread gc_requested flag BEFORE doing any state transitions
    // If this thread was created after request_gc_handshake(), its gc_requested flag
    // will be false even though global GC_REQUESTED is true. We must NOT participate
    // in rendezvous in this case, otherwise we'll:
    // 1. Transition to SAFEPOINT state (incorrectly)
    // 2. Decrement active_count (incorrectly)
    // 3. Return immediately (since gc_requested is false)
    // 4. Continue running while in SAFEPOINT state
    // This causes data race when collector accesses our heap concurrently.
    if !tcb.gc_requested.load(Ordering::Acquire) {
        return;
    }

    let old_state = tcb.state.compare_exchange(
        THREAD_STATE_EXECUTING,
        THREAD_STATE_SAFEPOINT,
        Ordering::AcqRel,
        Ordering::Acquire,
    );

    if old_state.is_err() {
        return;
    }

    // CRITICAL: Capture and store stack roots BEFORE decrementing active_count
    // This ensures that when collector sees active_count == 1, all threads have
    // already stored their complete stack roots. Otherwise, collector may read
    // empty/incomplete roots and miss live objects, causing memory corruption.
    let mut roots = Vec::new();
    unsafe {
        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
            roots.push(ptr as *const u8);
        });
    }
    *tcb.stack_roots.lock().unwrap() = roots;

    // Now decrement active_count to signal completion to collector
    thread_registry()
        .lock()
        .unwrap()
        .active_count
        .fetch_sub(1, Ordering::SeqCst);

    let mut guard = tcb.park_mutex.lock().unwrap();
    while tcb.gc_requested.load(Ordering::Acquire) {
        guard = tcb.park_cond.wait(guard).unwrap();
    }
}

/// Signal all threads waiting at safe points to resume.
///
/// # Panics
///
/// Panics if the thread registry lock is poisoned.
pub fn resume_all_threads() {
    let registry = thread_registry().lock().unwrap();
    let mut woken_count = 0;
    for tcb in &registry.threads {
        if tcb.state.load(Ordering::Acquire) == THREAD_STATE_SAFEPOINT {
            tcb.gc_requested.store(false, Ordering::Relaxed);
            tcb.park_cond.notify_all();
            tcb.state.store(THREAD_STATE_EXECUTING, Ordering::Release);
            woken_count += 1;
        }
    }
    // Restore active count only for threads that were woken up
    // CRITICAL FIX: Don't set active_count to threads.len(), only increment by woken_count
    // Setting to threads.len() was causing hangs by miscounting active threads
    registry
        .active_count
        .fetch_add(woken_count, std::sync::atomic::Ordering::SeqCst);
    drop(registry);

    // Clear global flag
    GC_REQUESTED.store(false, Ordering::Relaxed);
}

/// Global flag to ensure only one thread attempts GC handshake at a time
static GC_HANDSHAKE_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Request all threads to stop at the next safe point.
/// Returns true if this thread should become the collector.
///
/// # Panics
///
/// Panics if the thread registry lock is poisoned.
#[allow(dead_code)]
pub fn request_gc_handshake() -> bool {
    // Try to claim the handshake atomically - only one thread can proceed
    if GC_HANDSHAKE_CLAIMED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // Another thread already claimed the handshake
        return false;
    }

    let mut registry = thread_registry().lock().unwrap();

    // CRITICAL FIX: Auto-register current thread if not already registered.
    // This handles threads spawned via thread::spawn without explicit registration.
    let current_tcb = current_thread_control_block();
    if let Some(tcb) = &current_tcb {
        let is_registered = registry
            .threads
            .iter()
            .any(|reg_tcb| std::sync::Arc::ptr_eq(reg_tcb, tcb));
        if !is_registered {
            registry.register_thread(tcb.clone());
        }
    }

    // Wait for other threads to register (up to a timeout)
    // This handles the case where threads are spawned but haven't called collect() yet
    let start_time = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(50);
    let initial_count = registry.active_count.load(Ordering::Acquire);

    loop {
        let current_count = registry.active_count.load(Ordering::Acquire);
        // If count is stable for a bit, proceed
        if current_count > 1 && current_count == initial_count {
            break;
        }
        if start_time.elapsed() > timeout {
            break;
        }
        drop(registry);
        std::thread::sleep(std::time::Duration::from_micros(500));
        registry = thread_registry().lock().unwrap();
    }

    // Check if we're the only active thread (excluding newly spawned threads that haven't registered)
    // Actually, we should include ALL registered threads, not just active_count
    let registered_count = registry.threads.len();

    if registered_count != 1 {
        // Not the only registered thread - release the claim and return false
        // This means another thread should do the collection
        GC_HANDSHAKE_CLAIMED.store(false, Ordering::SeqCst);
        drop(registry);
        return false;
    }

    // We're the collector - set GC flags
    GC_REQUESTED.store(true, Ordering::SeqCst);

    // Set per-thread gc_requested flag for all threads
    for tcb in &registry.threads {
        tcb.gc_requested.store(true, Ordering::SeqCst);
    }

    true
}

/// Wait for GC to complete if a collection is in progress.
///
/// # Panics
///
/// Panics if the thread registry lock is poisoned.
#[allow(clippy::significant_drop_tightening)]
pub fn wait_for_gc_complete() {
    let Some(tcb) = current_thread_control_block() else {
        return;
    };

    let old_state = tcb.state.compare_exchange(
        THREAD_STATE_EXECUTING,
        THREAD_STATE_SAFEPOINT,
        Ordering::AcqRel,
        Ordering::Acquire,
    );

    if old_state.is_err() {
        return;
    }

    // CRITICAL: Capture and store stack roots BEFORE decrementing active_count
    // This ensures that when collector sees active_count == 1, all threads have
    // already stored their complete stack roots. Otherwise, collector may read
    // empty/incomplete roots and miss live objects, causing memory corruption.
    let mut roots = Vec::new();
    unsafe {
        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
            roots.push(ptr as *const u8);
        });
    }
    *tcb.stack_roots.lock().unwrap() = roots;

    // Now decrement active_count to signal completion to collector
    thread_registry()
        .lock()
        .unwrap()
        .active_count
        .fetch_sub(1, Ordering::SeqCst);

    let mut guard = tcb.park_mutex.lock().unwrap();
    while tcb.gc_requested.load(Ordering::Acquire) {
        guard = tcb.park_cond.wait(guard).unwrap();
    }
}

/// Clear the GC request flag after collection is complete.
///
/// # Panics
///
/// Panics if the thread registry lock is poisoned.
#[allow(dead_code)]
pub fn clear_gc_request() {
    let registry = thread_registry().lock().unwrap();
    for tcb in &registry.threads {
        tcb.gc_requested.store(false, Ordering::Relaxed);
    }
    drop(registry);
    GC_REQUESTED.store(false, Ordering::Relaxed);
}

/// Get a thread control block by its thread ID.
///
/// # Panics
///
/// Panics if the `THREAD_REGISTRY` lock is poisoned.
#[must_use]
pub fn get_thread_control_block_by_id(id: usize) -> Option<std::sync::Arc<ThreadControlBlock>> {
    let registry = thread_registry().lock().unwrap();
    if id < registry.threads.len() {
        Some(registry.threads[id].clone())
    } else {
        None
    }
}

/// Get the number of registered threads.
///
/// # Panics
///
/// Panics if the `THREAD_REGISTRY` lock is poisoned.
#[must_use]
pub fn get_thread_count() -> usize {
    thread_registry().lock().unwrap().threads.len()
}

/// Get the number of parallel GC worker threads.
#[cfg(feature = "parallel-gc")]
#[must_use]
pub fn get_worker_count() -> usize {
    std::env::var("RUDO_GC_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| std::cmp::max(4, num_cpus::get()))
}

#[cfg(feature = "parallel-gc")]
pub(crate) fn get_all_workers() -> std::sync::MutexGuard<'static, Vec<std::sync::Arc<SweepWorker>>>
{
    ALL_WORKERS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
}

/// Initialize the parallel worker pool.
///
/// # Panics
///
/// Panics if the `ALL_WORKERS` or `THREAD_REGISTRY` lock is poisoned.
#[cfg(feature = "parallel-gc")]
pub fn initialize_worker_pool() {
    let worker_count = get_worker_count();
    let mut workers = get_all_workers();

    if !workers.is_empty() {
        return;
    }

    let _registry = thread_registry().lock().unwrap();

    for i in 0..worker_count {
        let worker = std::sync::Arc::new(SweepWorker {
            id: i,
            status: AtomicUsize::new(SWEEPER_NONE),
            work_cond: Condvar::new(),
            done_cond: Condvar::new(),
            mutex: Mutex::new(()),
            tcbs: Mutex::new(Vec::new()),
        });

        std::thread::spawn({
            let worker = std::sync::Arc::clone(&worker);
            move || sweeper_thread_main(worker)
        });

        workers.push(worker);
    }
}

#[cfg(feature = "parallel-gc")]
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn sweeper_thread_main(worker: std::sync::Arc<SweepWorker>) {
    loop {
        let mut guard = worker.mutex.lock().unwrap();

        while worker.status.load(Ordering::Acquire) != SWEEPER_SWEEPING {
            if worker.status.load(Ordering::Acquire) == SWEEPER_NONE {
                drop(guard);
                std::thread::sleep(std::time::Duration::from_micros(100));
                guard = worker.mutex.lock().unwrap();
                continue;
            }

            if worker.status.load(Ordering::Acquire) == SWEEPER_WAITING_FOR_WORK {
                guard = worker.work_cond.wait(guard).unwrap();
            }
        }

        drop(guard);

        let (workers_len, thread_count) = {
            let workers = get_all_workers();
            let registry = thread_registry().lock().unwrap();
            (workers.len(), registry.threads.len())
        };

        loop {
            let mut local_done = true;
            let mut has_remote = false;

            let tcbs: Vec<_> = worker.tcbs.lock().unwrap().clone();
            for tcb in &tcbs {
                while let Some(gc_ptr) = tcb.pop_local_mark() {
                    if !unsafe { is_gc_pointer(gc_ptr) } {
                        continue;
                    }

                    let owner_id = unsafe { ptr_to_page_owner(gc_ptr) };

                    if owner_id == worker.id {
                        let heap_ptr = tcb.heap.get() as *mut LocalHeap;
                        if let Some(gc_box) = unsafe { find_gc_box_from_ptr(&*heap_ptr, gc_ptr) } {
                            let registry_ref = thread_registry();
                            let registry_arc = std::sync::Arc::new(std::sync::Mutex::new(
                                registry_ref.lock().unwrap().clone(),
                            ));
                            let mut visitor = GcVisitor::new_parallel(
                                VisitorKind::Major,
                                worker.id,
                                registry_arc,
                            );
                            unsafe {
                                crate::gc::mark_object(gc_box, &mut visitor);
                            }
                        }
                    } else if owner_id < thread_count {
                        let target_worker_id = owner_id % workers_len;
                        tcb.queue_remote_forward(gc_ptr, target_worker_id);
                    }
                }

                let received = tcb.drain_remote_inbox_to_local();
                if received > 0 {
                    has_remote = true;
                }

                tcb.flush_send_buffer();
            }

            if has_remote {
                continue;
            }

            let tcbs_check: Vec<_> = worker.tcbs.lock().unwrap().clone();
            for tcb in &tcbs_check {
                if !tcb.is_local_mark_stack_empty() {
                    local_done = false;
                    break;
                }
            }

            if local_done {
                // CRITICAL FIX: Re-check remote inbox after confirming local stack is empty.
                // There is a race window where new work could arrive between:
                // 1. drain_remote_inbox_to_local() returning 0
                // 2. is_local_mark_stack_empty() returning true
                // 3. This check
                // The new work would be in the remote inbox, not the local stack yet.
                let tcbs_recheck: Vec<_> = worker.tcbs.lock().unwrap().clone();
                let mut any_pending = false;
                for tcb in &tcbs_recheck {
                    let inbox = tcb.remote_inbox.lock().unwrap();
                    if !inbox.is_empty() {
                        any_pending = true;
                        break;
                    }
                }
                if any_pending {
                    continue; // New work arrived, process it
                }
                break;
            }
        }

        NUM_RUNNING_SWEEPERS.fetch_sub(1, Ordering::SeqCst);

        let _guard = SWEEP_MUTEX.lock().unwrap();
        let remaining = NUM_RUNNING_SWEEPERS.load(Ordering::SeqCst);

        let mut any_pending = false;
        let tcbs_pending: Vec<_> = worker.tcbs.lock().unwrap().clone();
        for tcb in &tcbs_pending {
            if !tcb.remote_inbox.lock().unwrap().is_empty() {
                any_pending = true;
                break;
            }
        }

        if remaining == 0 && !any_pending {
            let workers = get_all_workers().clone();
            for w in &workers {
                let _g = w.mutex.lock().unwrap();
                w.status.store(SWEEPER_READY, Ordering::Release);
                w.work_cond.notify_all();
            }

            let g = worker.work_cond.wait(worker.mutex.lock().unwrap()).unwrap();
            drop(g);
        } else {
            if any_pending {
                // CRITICAL FIX: Re-check after acquiring lock. New work might have arrived.
                let tcbs_recheck: Vec<_> = worker.tcbs.lock().unwrap().clone();
                let mut still_pending = false;
                for tcb in &tcbs_recheck {
                    if !tcb.remote_inbox.lock().unwrap().is_empty() {
                        still_pending = true;
                        break;
                    }
                }
                if still_pending {
                    NUM_RUNNING_SWEEPERS.fetch_add(1, Ordering::SeqCst);
                    continue; // Go back to processing loop
                }
            }
            // No pending work, wait for more
            worker
                .status
                .store(SWEEPER_WAITING_FOR_WORK, Ordering::Release);
            let g = worker.work_cond.wait(worker.mutex.lock().unwrap()).unwrap();
            drop(g);
        }
    }
}

/// Get the list of all thread control blocks for scanning.
///
/// # Panics
///
/// Panics if the thread registry lock is poisoned.
#[allow(dead_code)]
#[must_use]
pub fn get_all_thread_control_blocks() -> Vec<std::sync::Arc<ThreadControlBlock>> {
    thread_registry().lock().unwrap().threads.clone()
}

/// Get stack roots from a thread control block.
/// Returns the captured stack roots and clears the buffer.
///
/// # Panics
///
/// Panics if the stack roots lock is poisoned.
#[allow(dead_code)]
pub fn take_stack_roots(tcb: &ThreadControlBlock) -> Vec<*const u8> {
    std::mem::take(&mut *tcb.stack_roots.lock().unwrap())
}

// ============================================================================
// Constants
// ============================================================================

/// Size of each memory page (4KB aligned).
pub const PAGE_SIZE: usize = 4096;

/// Target address for heap allocation (Address Space Coloring).
/// We aim for `0x6000_0000_0000` on 64-bit systems.
#[cfg(target_pointer_width = "64")]
pub const HEAP_HINT_ADDRESS: usize = 0x6000_0000_0000;

/// Target address for heap allocation on 32-bit systems.
#[cfg(target_pointer_width = "32")]
pub const HEAP_HINT_ADDRESS: usize = 0x4000_0000;

/// Mask for extracting page address from a pointer.
pub const PAGE_MASK: usize = !(PAGE_SIZE - 1);

/// Magic number for validating GC pages ("RUDG" in ASCII).
pub const MAGIC_GC_PAGE: u32 = 0x5255_4447;

/// Size classes for object allocation.
/// Objects are routed to the smallest size class that fits them.
#[allow(dead_code)]
pub const SIZE_CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// Objects larger than this go to the Large Object Space.
pub const MAX_SMALL_OBJECT_SIZE: usize = 2048;

#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
pub const BATCH_FORWARD_SIZE: usize = 64;

#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
static SWEEP_MUTEX: Mutex<()> = Mutex::new(());

#[cfg(feature = "parallel-gc")]
#[allow(missing_docs)]
pub static NUM_RUNNING_SWEEPERS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "parallel-gc")]
pub(crate) static ALL_WORKERS: std::sync::OnceLock<
    std::sync::Mutex<Vec<std::sync::Arc<SweepWorker>>>,
> = std::sync::OnceLock::new();

// ============================================================================
// PageHeader - Metadata at the start of each page
// ============================================================================

/// Metadata stored at the beginning of each page.
///
/// This header enables O(1) lookup of object information from any pointer
/// within the page using simple alignment operations.
#[repr(C)]
pub struct PageHeader {
    /// Magic number to validate this is a GC page.
    pub magic: u32,
    /// Size of each object slot in bytes (u32 to support multi-page large objects).
    pub block_size: u32,
    /// Maximum number of objects in this page.
    pub obj_count: u16,
    /// Offset from the start of the page to the first object.
    pub header_size: u16,
    /// Generation index (for future generational GC).
    pub generation: u8,
    /// Bitflags (`is_large_object`, `is_dirty`, etc.).
    pub flags: u8,
    /// Padding for alignment.
    pub padding: [u8; 2],
    /// Owner thread ID for parallel GC (Remote Mentions).
    /// This identifies which thread owns this page for marking.
    /// usize to accommodate thread IDs from different threads.
    pub owner_id: usize,
    /// Bitmap of marked objects (one bit per slot).
    /// Size depends on `obj_count`, but we reserve space for max possible.
    pub mark_bitmap: [u64; 4], // 256 bits = enough for smallest size class (16 bytes)
    /// Bitmap of dirty objects (one bit per slot).
    /// Used for generational GC to track old objects that point to young objects.
    pub dirty_bitmap: [u64; 4],
    /// Bitmap of allocated objects (one bit per slot).
    /// Used to distinguish between newly unreachable and already free slots.
    pub allocated_bitmap: [u64; 4],
    /// Index of first free slot in free list.
    pub free_list_head: Option<u16>,
}

impl PageHeader {
    /// Calculate the header size, rounded up to block alignment.
    #[must_use]
    pub const fn header_size(block_size: usize) -> usize {
        let base = std::mem::size_of::<Self>();
        // For small objects, block_size is a power-of-two size class (16, 32, ..., 2048).
        // For large objects, block_size is the actual size (which might not be a power-of-two).
        if block_size > 0 && block_size.is_power_of_two() && block_size <= MAX_SMALL_OBJECT_SIZE {
            (base + block_size - 1) & !(block_size - 1)
        } else {
            // For large objects, align to at least 16 bytes (standard alignment for GcBox header).
            // Note: alloc_large will handle stricter alignment if needed.
            (base + 15) & !15
        }
    }

    /// Calculate maximum objects per page for a given block size.
    #[must_use]
    pub const fn max_objects(block_size: usize) -> usize {
        (PAGE_SIZE - Self::header_size(block_size)) / block_size
    }

    /// Check if an object at the given index is marked.
    #[must_use]
    pub const fn is_marked(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.mark_bitmap[word] & (1 << bit)) != 0
    }

    /// Set the mark bit for an object at the given index.
    pub const fn set_mark(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.mark_bitmap[word] |= 1 << bit;
    }

    /// Clear the mark bit for an object at the given index.
    #[allow(dead_code)]
    pub const fn clear_mark(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.mark_bitmap[word] &= !(1 << bit);
    }

    /// Clear all mark bits.
    pub const fn clear_all_marks(&mut self) {
        self.mark_bitmap = [0; 4];
    }

    /// Check if an object at the given index is dirty.
    #[must_use]
    pub const fn is_dirty(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.dirty_bitmap[word] & (1 << bit)) != 0
    }

    /// Set the dirty bit for an object at the given index.
    pub const fn set_dirty(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word] |= 1 << bit;
    }

    /// Clear the dirty bit for an object at the given index.
    #[allow(dead_code)]
    pub const fn clear_dirty(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word] &= !(1 << bit);
    }

    /// Clear all dirty bits.
    pub const fn clear_all_dirty(&mut self) {
        self.dirty_bitmap = [0; 4];
    }

    /// Check if an object at the given index is allocated.
    #[must_use]
    pub const fn is_allocated(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.allocated_bitmap[word] & (1 << bit)) != 0
    }

    /// Set the allocated bit for an object at the given index.
    pub const fn set_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word] |= 1 << bit;
    }

    /// Clear the allocated bit for an object at the given index.
    pub const fn clear_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word] &= !(1 << bit);
    }

    /// Clear all allocated bits.
    pub const fn clear_all_allocated(&mut self) {
        self.allocated_bitmap = [0; 4];
    }
}

// ============================================================================
// Segment - Size-class based memory pool
// ============================================================================

// ============================================================================
// Tlab - Thread-Local Allocation Buffer
// ============================================================================

/// A Thread-Local Allocation Buffer (TLAB) for a specific size class.
///
/// This structure tracks the current page being allocated from.
/// It does NOT own the pages; the `LocalHeap` owns the vector of pages.
pub struct Tlab {
    /// Pointer to the next free byte in the current page.
    pub bump_ptr: *mut u8,
    /// Pointer to the end of the allocation region in the current page.
    pub bump_end: *const u8,
    /// The page currently being used for allocation.
    pub current_page: Option<NonNull<PageHeader>>,
}

impl Tlab {
    /// Create a new empty TLAB.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bump_ptr: std::ptr::null_mut(),
            bump_end: std::ptr::null(),
            current_page: None,
        }
    }

    /// Try to allocate from the TLAB (Fast Path).
    ///
    /// Returns `Some(ptr)` if successful, `None` if the TLAB is exhausted.
    #[inline]
    pub fn alloc(&mut self, block_size: usize) -> Option<NonNull<u8>> {
        check_safepoint();
        let ptr = self.bump_ptr;
        // Check if we have enough space.
        // We use wrapping_add and compare as usize to avoid UB with ptr.add(block_size)
        // if it were to go past the page boundary.
        if !ptr.is_null() && (ptr as usize).wrapping_add(block_size) <= self.bump_end as usize {
            // SAFETY: ptr is valid and within bounds as checked above
            unsafe {
                self.bump_ptr = ptr.add(block_size);

                // We need to mark the object as allocated in the bitmap.
                // This adds a bit of overhead to the fast path.
                // In true bump-pointer systems, we might defer this or assume all processed objects are allocated.
                // But for accurate sweeping, we need it.
                // Optimally, we would do this batch-wise or rely on the fact that TLAB pages are young
                // and young gen collection just copies/evacuates, so marking 'allocated' might strictly be needed
                // only if we do mark-sweep on young gen (which we do currently).
                if let Some(mut page) = self.current_page {
                    let header = page.as_mut();
                    let header_size = PageHeader::header_size(block_size);
                    let page_start = page.as_ptr() as usize;
                    let offset = ptr as usize - (page_start + header_size);
                    let idx = offset / block_size;
                    header.set_allocated(idx);
                }

                return Some(NonNull::new_unchecked(ptr));
            }
        }
        None
    }
}

impl Default for Tlab {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// SizeClass trait - Compile-time size class routing
// ============================================================================

/// Trait for computing size class at compile time.
#[allow(dead_code)]
pub trait SizeClass {
    /// The size of the type.
    const SIZE: usize;
    /// The size class for this type (smallest class that fits).
    const CLASS: usize;
    /// Index into the segments array.
    const CLASS_INDEX: usize;
}

impl<T> SizeClass for T {
    const SIZE: usize = std::mem::size_of::<T>();
    const CLASS: usize = compute_size_class(std::mem::size_of::<T>());
    const CLASS_INDEX: usize = compute_class_index(std::mem::size_of::<T>());
}

/// Compute the size class for a given size.
#[allow(dead_code)]
const fn compute_size_class(size: usize) -> usize {
    if size <= 16 {
        16
    } else if size <= 32 {
        32
    } else if size <= 64 {
        64
    } else if size <= 128 {
        128
    } else if size <= 256 {
        256
    } else if size <= 512 {
        512
    } else if size <= 1024 {
        1024
    } else {
        2048
    }
}

/// Compute the index into the segments array.
const fn compute_class_index(size: usize) -> usize {
    if size <= 16 {
        0
    } else if size <= 32 {
        1
    } else if size <= 64 {
        2
    } else if size <= 128 {
        3
    } else if size <= 256 {
        4
    } else if size <= 512 {
        5
    } else if size <= 1024 {
        6
    } else {
        7
    }
}

// ============================================================================
// GlobalSegmentManager - Shared memory manager
// ============================================================================

/// Shared memory manager coordinating all pages.
pub struct GlobalSegmentManager {
    /// Pages that are free and can be handed out to threads.
    /// For now, we don't maintain a free list of pages, we just allocate fresh ones.
    /// This is where we would put pages returned by thread termination or GC.
    #[allow(dead_code)]
    free_pages: Vec<NonNull<PageHeader>>,

    /// Quarantined pages (bad stack conflict).
    quarantined: Vec<Mmap>,

    /// Large object tracking map.
    /// Map from page address to its corresponding large object head, size, and `header_size`.
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,
}

/// Global singleton for the segment manager.
static SEGMENT_MANAGER: OnceLock<Mutex<GlobalSegmentManager>> = OnceLock::new();

/// Access the global segment manager.
pub fn segment_manager() -> &'static Mutex<GlobalSegmentManager> {
    SEGMENT_MANAGER.get_or_init(|| Mutex::new(GlobalSegmentManager::new()))
}

impl GlobalSegmentManager {
    /// Create a new segment manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            free_pages: Vec::new(),
            quarantined: Vec::new(),
            large_object_map: HashMap::new(),
        }
    }

    /// Allocate a new page safely.
    ///
    /// This moves the logic from `GlobalHeap::allocate_safe_page` to here.
    ///
    /// # Panics
    ///
    /// Panics if the OS fails to map the requested memory.
    pub fn allocate_page(&mut self, size: usize, boundary: usize) -> (NonNull<u8>, usize) {
        // Mask to hide our own variables from conservative stack scanning (registers)
        const MASK: usize = 0x5555_5555_5555_5555;

        loop {
            // 1. Request memory from OS with Address Space Coloring hint
            // Boxing the Mmap moves the raw pointer value to the heap,
            // so it doesn't appear on the stack (only the pointer to the box does).
            let mmap = Box::new(unsafe {
                MmapOptions::new()
                    .len(size)
                    .with_hint(HEAP_HINT_ADDRESS)
                    .map_anon()
                    .unwrap_or_else(|e| panic!("Failed to map memory: {e}"))
            });

            // 2. Check for False Roots on Stack
            // Use helper to keep `ptr` scope small
            let (masked_start, masked_end) = Self::calculate_masked_range(&mmap, size, MASK);

            // Clear registers to ensure `ptr` doesn't linger in callee-saved registers.
            unsafe { crate::stack::clear_registers() };

            let conflict_found =
                Self::check_stack_conflict(masked_start, masked_end, MASK, boundary);

            // 3. Handle conflict
            if conflict_found {
                // Quarantine this page.
                self.quarantined.push(*mmap);
                continue;
            }

            // 4. Success! Convert to raw pointer and return.
            let (raw_ptr, len) = mmap.into_raw();
            return (unsafe { NonNull::new_unchecked(raw_ptr) }, len);
        }
    }

    /// Helper to calculate masked range.
    #[inline(never)]
    fn calculate_masked_range(mmap: &Mmap, size: usize, mask: usize) -> (usize, usize) {
        let ptr = mmap.ptr() as usize;
        (ptr ^ mask, (ptr + size) ^ mask)
    }

    /// Check if any value on the current stack falls within [start, end).
    /// Ignores stack slots below `boundary` (Assume Allocator Frame), UNLESS it is a Register.
    fn check_stack_conflict(
        masked_start: usize,
        masked_end: usize,
        mask: usize,
        boundary: usize,
    ) -> bool {
        let mut found = false;
        // Use the stack module to spill registers and scan stack
        unsafe {
            crate::stack::spill_registers_and_scan(|scan_ptr, slot_addr, is_reg| {
                if !is_reg {
                    // It is a stack slot. Filter based on boundary.
                    if slot_addr < boundary {
                        return;
                    }
                }

                // It is a user root (stack or register). Check it.
                let start = masked_start ^ mask;
                let end = masked_end ^ mask;
                if scan_ptr >= start && scan_ptr < end {
                    found = true;
                }
            });
        }
        found
    }
}

// SAFETY: GlobalSegmentManager owns the pointers and Mmaps.
// Access is synchronized via the Mutex wrapper.
unsafe impl Send for GlobalSegmentManager {}
unsafe impl Sync for GlobalSegmentManager {}

impl Default for GlobalSegmentManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// LocalHeap - Thread-Local memory manager
// ============================================================================

/// Thread-local memory manager.
///
/// Handles allocation requests from the thread, using TLABs for speed
/// and getting new pages from the `GlobalSegmentManager`.
pub struct LocalHeap {
    /// TLAB for 16-byte size class.
    pub tlab_16: Tlab,
    /// TLAB for 32-byte size class.
    pub tlab_32: Tlab,
    /// TLAB for 64-byte size class.
    pub tlab_64: Tlab,
    /// TLAB for 128-byte size class.
    pub tlab_128: Tlab,
    /// TLAB for 256-byte size class.
    pub tlab_256: Tlab,
    /// TLAB for 512-byte size class.
    pub tlab_512: Tlab,
    /// TLAB for 1024-byte size class.
    pub tlab_1024: Tlab,
    /// TLAB for 2048-byte size class.
    pub tlab_2048: Tlab,

    /// All pages owned by this heap (small and large).
    /// Used for sweeping.
    pub pages: Vec<NonNull<PageHeader>>,

    /// Set of small page addresses for O(1) safety checks during conservative scanning.
    pub small_pages: HashSet<usize>,

    /// Map from page address to its corresponding large object head.
    /// Still useful for interior pointers.
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,

    /// Bytes allocated in young generation.
    young_allocated: usize,
    /// Bytes allocated in old generation.
    old_allocated: usize,
    /// Minimum address managed by this heap.
    min_addr: usize,
    /// Maximum address managed by this heap.
    max_addr: usize,
}

impl LocalHeap {
    /// Create a new empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tlab_16: Tlab::new(),
            tlab_32: Tlab::new(),
            tlab_64: Tlab::new(),
            tlab_128: Tlab::new(),
            tlab_256: Tlab::new(),
            tlab_512: Tlab::new(),
            tlab_1024: Tlab::new(),
            tlab_2048: Tlab::new(),
            pages: Vec::new(),
            small_pages: HashSet::new(),
            large_object_map: HashMap::new(),
            young_allocated: 0,
            old_allocated: 0,
            min_addr: usize::MAX,
            max_addr: 0,
        }
    }

    /// Update the address range of the heap.
    const fn update_range(&mut self, addr: usize, size: usize) {
        if addr < self.min_addr {
            self.min_addr = addr;
        }
        if addr + size > self.max_addr {
            self.max_addr = addr + size;
        }
    }

    // deallocate_pages removed as it is unused (using Mmap directly in gc.rs)
    /// Check if an address is within the heap's range.
    #[must_use]
    pub const fn is_in_range(&self, addr: usize) -> bool {
        addr >= self.min_addr && addr < self.max_addr
    }

    /// Allocate space for a value of type T.
    ///
    /// Returns a pointer to uninitialized memory.
    ///
    /// # Panics
    ///
    /// Panics if the type's alignment exceeds the size class alignment.
    /// This should be extremely rare in practice since size classes are
    /// powers of two starting at 16.
    pub fn alloc<T>(&mut self) -> NonNull<u8> {
        let size = std::mem::size_of::<T>();
        let align = std::mem::align_of::<T>();
        // All new allocations start in young generation
        self.young_allocated += size;

        if size > MAX_SMALL_OBJECT_SIZE {
            return self.alloc_large(size, align);
        }

        // Validate alignment - size class must satisfy alignment requirement
        let size_class = compute_size_class(size);
        assert!(
            size_class >= align,
            "Type alignment ({align}) exceeds size class ({size_class}). \
             Consider using a larger wrapper type."
        );

        // Try TLAB allocation
        let class_index = compute_class_index(size);
        let ptr_opt = match class_index {
            0 => self.tlab_16.alloc(16),
            1 => self.tlab_32.alloc(32),
            2 => self.tlab_64.alloc(64),
            3 => self.tlab_128.alloc(128),
            4 => self.tlab_256.alloc(256),
            5 => self.tlab_512.alloc(512),
            6 => self.tlab_1024.alloc(1024),
            _ => self.tlab_2048.alloc(2048),
        };

        if let Some(ptr) = ptr_opt {
            // Update heap range for conservative scanning
            self.update_range(ptr.as_ptr() as usize & PAGE_MASK, PAGE_SIZE);
            return ptr;
        }

        // Try to allocate from existing pages' free lists
        if let Some(ptr) = self.alloc_from_free_list(class_index) {
            self.update_range(ptr.as_ptr() as usize & PAGE_MASK, PAGE_SIZE);
            return ptr;
        }

        // Slow path: Refill TLAB and retry
        let ptr = self.alloc_slow(size, class_index);

        self.update_range(ptr.as_ptr() as usize & PAGE_MASK, PAGE_SIZE);
        ptr
    }

    /// Try to allocate from the free list of an existing page.
    fn alloc_from_free_list(&self, class_index: usize) -> Option<NonNull<u8>> {
        let block_size = SIZE_CLASSES[class_index];
        for page_ptr in &self.pages {
            unsafe {
                let header = page_ptr.as_ptr();
                // We only care about regular pages (not large objects)
                if ((*header).flags & 0x01) == 0
                    && (*header).block_size as usize == block_size
                    && (*header).free_list_head.is_some()
                {
                    let idx = (*header).free_list_head.unwrap();
                    let h_size = (*header).header_size as usize;
                    let obj_ptr = page_ptr
                        .as_ptr()
                        .cast::<u8>()
                        .add(h_size + (idx as usize * block_size));

                    // Popping from free list: read the next pointer stored in the slot.
                    // SAFETY: sweep_page (copy_sweep_logic) ensures this is a valid Option<u16>.
                    // We use read_unaligned to avoid potential alignment issues with the cast.
                    let next_head = obj_ptr.cast::<Option<u16>>().read_unaligned();
                    (*header).free_list_head = next_head;

                    // Mark as allocated so it's tracked during sweep
                    (*header).set_allocated(idx as usize);

                    return Some(NonNull::new_unchecked(obj_ptr));
                }
            }
        }
        None
    }

    #[inline(never)]
    fn alloc_slow(&mut self, _size: usize, class_index: usize) -> NonNull<u8> {
        check_safepoint();
        let block_size = match class_index {
            0 => 16,
            1 => 32,
            2 => 64,
            3 => 128,
            4 => 256,
            5 => 512,
            6 => 1024,
            _ => 2048,
        };

        // 1. Request new page from global manager
        // Create boundary to filter out our own stack frame
        let marker = 0;
        let boundary = std::ptr::addr_of!(marker) as usize;

        let (ptr, _) = segment_manager()
            .lock()
            .unwrap()
            .allocate_page(crate::heap::PAGE_SIZE, boundary);

        // 2. Initialize Page Header
        // SAFETY: ptr is page-aligned
        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<PageHeader>();
        let obj_count = PageHeader::max_objects(block_size);
        let h_size = PageHeader::header_size(block_size);

        // Get current thread ID for page ownership
        let thread_id = HEAP.try_with(|heap| heap.tcb.id()).unwrap_or(0);

        unsafe {
            header.as_ptr().write(PageHeader {
                magic: MAGIC_GC_PAGE,
                #[allow(clippy::cast_possible_truncation)]
                block_size: block_size as u32,
                #[allow(clippy::cast_possible_truncation)]
                obj_count: obj_count as u16,
                #[allow(clippy::cast_possible_truncation)]
                header_size: h_size as u16,
                generation: 0,
                flags: 0,
                padding: [0; 2],
                owner_id: thread_id,
                mark_bitmap: [0; 4],
                dirty_bitmap: [0; 4],
                allocated_bitmap: [0; 4],
                free_list_head: None,
            });

            // Initialize all slots with no-op drop
            for i in 0..obj_count {
                let obj_ptr = ptr.as_ptr().add(h_size + (i * block_size));
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                std::ptr::addr_of_mut!((*gc_box_ptr).drop_fn)
                    .write(crate::ptr::GcBox::<()>::no_op_drop);
                std::ptr::addr_of_mut!((*gc_box_ptr).trace_fn)
                    .write(crate::ptr::GcBox::<()>::no_op_trace);
            }
        }

        // 3. Update LocalHeap pages list
        self.pages.push(header);
        self.small_pages.insert(ptr.as_ptr() as usize);

        // 4. Update Tlab
        let tlab = match class_index {
            0 => &mut self.tlab_16,
            1 => &mut self.tlab_32,
            2 => &mut self.tlab_64,
            3 => &mut self.tlab_128,
            4 => &mut self.tlab_256,
            5 => &mut self.tlab_512,
            6 => &mut self.tlab_1024,
            _ => &mut self.tlab_2048,
        };

        tlab.current_page = Some(header);
        unsafe {
            tlab.bump_ptr = ptr.as_ptr().add(h_size);
            // bump_end is the end of the last object that fits in the page.
            tlab.bump_end = ptr.as_ptr().add(h_size + obj_count * block_size);
        }

        // 5. Retry allocation (guaranteed to succeed now)
        tlab.alloc(block_size).unwrap()
    }

    /// Allocate a large object (> 2KB).
    ///
    /// # Panics
    ///
    /// Panics if the alignment requirement exceeds `PAGE_SIZE`.
    fn alloc_large(&mut self, size: usize, align: usize) -> NonNull<u8> {
        // Check for pending GC request - large object allocation can block GC
        check_safepoint();

        // Validate alignment - page alignment (4096) should satisfy most types
        assert!(
            PAGE_SIZE >= align,
            "Type alignment ({align}) exceeds page size ({PAGE_SIZE}). \
             Such extreme alignment requirements are not supported."
        );

        // For large objects, allocate dedicated pages
        // The header must be followed by padding to satisfy the object's alignment.
        let base_h_size = PageHeader::header_size(size);
        let h_size = (base_h_size + align - 1) & !(align - 1);
        let total_size = h_size + size;
        let pages_needed = total_size.div_ceil(PAGE_SIZE);
        let alloc_size = pages_needed * PAGE_SIZE;

        // Use safe allocation logic
        // Create boundary to filter out our own stack frame
        let marker = 0;
        let boundary = std::ptr::addr_of!(marker) as usize;
        let (ptr, _) = segment_manager()
            .lock()
            .unwrap()
            .allocate_page(alloc_size, boundary);

        // ptr is NonNull<u8> already check for null logic inside allocate_safe_page

        // Get current thread ID for page ownership
        let thread_id = HEAP.try_with(|heap| heap.tcb.id()).unwrap_or(0);

        // SAFETY: ptr is page-aligned, which is more strict than PageHeader's alignment.
        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<PageHeader>();
        // SAFETY: We just allocated this memory
        unsafe {
            header.as_ptr().write(PageHeader {
                magic: MAGIC_GC_PAGE,
                #[allow(clippy::cast_possible_truncation)]
                block_size: size as u32, // Store actual size for large objects (now u32)
                obj_count: 1,
                #[allow(clippy::cast_possible_truncation)]
                header_size: h_size as u16,
                generation: 0,
                flags: 0x01, // Mark as large object
                padding: [0; 2],
                owner_id: thread_id,
                mark_bitmap: [0; 4],
                dirty_bitmap: [0; 4],
                allocated_bitmap: [0; 4],
                free_list_head: None,
            });
            // Mark the single object as allocated
            (*header.as_ptr()).set_allocated(0);
        }

        let page_ptr = header; // header is NonNull
        self.pages.push(page_ptr); // Push to unified pages list

        // Register all pages of this large object in the map for interior pointer support.
        // This allows find_gc_box_from_ptr to find the head GcBox from any interior pointer.
        // We register this in BOTH local and global map for now?
        // Actually, interior pointers need to be found from ANY thread potentially...
        // But conservative stack scanning is usually thread-local stacks finding objects.
        // If one thread scans stack and finds ptr to object alloc'd by another thread,
        // it needs the global map if that object spans multiple pages.
        // For Phase 1, large_object_map is duplicated or split responsibility.
        // Let's Register in LOCAL map for now as GlobalHeap still exists.
        // GlobalSegmentManager also has a map, maybe we should register there too?
        // For strict TLAB, large objects are often alloc'd directly from Global.
        // Let's verify: GlobalSegmentManager has `large_object_map`.
        // We should probably optimize this later, but for parity:
        let header_addr = header.as_ptr() as usize;
        for p in 0..pages_needed {
            let page_addr = header_addr + (p * PAGE_SIZE);
            self.large_object_map
                .insert(page_addr, (header_addr, size, h_size));
            // Register in global manager too?
            segment_manager()
                .lock()
                .unwrap()
                .large_object_map
                .insert(page_addr, (header_addr, size, h_size));
        }

        // Update heap range for conservative scanning
        self.update_range(header_addr, alloc_size);

        let gc_box_ptr = unsafe { ptr.as_ptr().add(h_size) };
        unsafe { NonNull::new_unchecked(gc_box_ptr) }
    }

    /// Get total bytes allocated.
    #[must_use]
    pub const fn total_allocated(&self) -> usize {
        self.young_allocated + self.old_allocated
    }

    /// Get bytes allocated in young generation.
    #[must_use]
    pub const fn young_allocated(&self) -> usize {
        self.young_allocated
    }

    /// Get bytes allocated in old generation.
    #[must_use]
    pub const fn old_allocated(&self) -> usize {
        self.old_allocated
    }

    /// Update allocation counters given a change in young/old bytes.
    /// This is used by the collector during promotion and sweeping.
    pub const fn update_allocated_bytes(&mut self, young: usize, old: usize) {
        self.young_allocated = young;
        self.old_allocated = old;
    }

    /// Iterate over all pages.
    pub fn all_pages(&self) -> impl Iterator<Item = NonNull<PageHeader>> + '_ {
        self.pages.iter().copied()
    }

    /// Get large object pages (now just filtered from all pages, or tracked if we want).
    /// If we need specifically large objects, we can check flags.
    /// Or we can keep `large_objects` list if needed for the map management.
    /// Plan said "Remove vector of pages from Segment/Tlab".
    /// Plan also said "Modify `LocalHeap`... pages: Vec<`NonNull`<PageHeader>>".
    /// Let's stick to `self.pages` having everything.
    #[must_use]
    pub fn large_object_pages(&self) -> Vec<NonNull<PageHeader>> {
        self.pages
            .iter()
            .filter(|p| unsafe { (p.as_ptr().read().flags & 0x01) != 0 })
            .copied()
            .collect()
    }

    /// Get mutable access to large object pages (for sweep phase).
    /// This signature is tricky if we don't have a separate vec.
    /// But sweep functions in `gc.rs` usually iterate.
    /// Let's leave this but maybe change return type or deprecate it.
    /// Actually, `gc.rs` uses `heap.large_object_pages()`.
    /// We should probably update `gc.rs` to just use `all_pages` and check flags internally?
    /// Or just return a new Vec as above.
    #[allow(dead_code)]
    pub fn large_object_pages_mut(&mut self) -> Vec<NonNull<PageHeader>> {
        self.pages
            .iter()
            .filter(|p| unsafe { (p.as_ptr().read().flags & 0x01) != 0 })
            .copied()
            .collect()
    }

    /// Get the size class index for a type.
    ///
    /// This is useful for debugging and verifying `BiBOP` routing.
    ///
    /// # Returns
    ///
    /// - `Some(index)` - Size class index (0-7) for small objects
    /// - `None` - Type is a large object (> 2KB)
    #[must_use]
    #[allow(dead_code)]
    pub const fn size_class_for<T>() -> Option<usize> {
        let size = std::mem::size_of::<T>();
        if size > MAX_SMALL_OBJECT_SIZE {
            None
        } else {
            Some(compute_class_index(size))
        }
    }

    /// Get the segment index and size class name for debugging.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rudo_gc::heap::LocalHeap;
    ///
    /// let (class, name) = LocalHeap::debug_size_class::<u64>();
    /// assert_eq!(name, "16-byte");
    /// ```
    #[must_use]
    #[allow(dead_code)]
    pub const fn debug_size_class<T>() -> (usize, &'static str) {
        let size = std::mem::size_of::<T>();
        let class = compute_size_class(size);
        let name = match class {
            16 => "16-byte",
            32 => "32-byte",
            64 => "64-byte",
            128 => "128-byte",
            256 => "256-byte",
            512 => "512-byte",
            1024 => "1024-byte",
            2048 => "2048-byte",
            _ => "large-object",
        };
        (class, name)
    }
}

impl Default for LocalHeap {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LocalHeap {
    fn drop(&mut self) {
        // When a thread terminates, its LocalHeap is dropped.
        // We must unmap all pages owned by this heap to avoid memory leaks.
        for page_ptr in &self.pages {
            unsafe {
                let header = page_ptr.as_ptr();
                // Validate this is still a GC page before attempting to read metadata
                if (*header).magic != MAGIC_GC_PAGE {
                    continue;
                }

                let is_large = ((*header).flags & 0x01) != 0;
                let block_size = (*header).block_size as usize;
                let header_size = (*header).header_size as usize;

                let (alloc_size, pages_needed) = if is_large {
                    let total_size = header_size + block_size;
                    let pages = total_size.div_ceil(PAGE_SIZE);
                    (pages * PAGE_SIZE, pages)
                } else {
                    (PAGE_SIZE, 1)
                };

                // Unregister from global large_object_map if it was a large object.
                // This is important because other threads might still be scanning
                // their stacks and could find an interior pointer to this memory.
                if is_large {
                    let mut manager = segment_manager()
                        .lock()
                        .expect("segment manager lock poisoned");
                    let header_addr = header as usize;
                    for p in 0..pages_needed {
                        let page_addr = header_addr + (p * PAGE_SIZE);
                        manager.large_object_map.remove(&page_addr);
                    }
                }

                // Actually unmap the memory.
                // sys_alloc::Mmap::from_raw recreate the Mmap object, which will
                // unmap the memory when it's dropped at the end of this scope.
                sys_alloc::Mmap::from_raw(header.cast::<u8>(), alloc_size);
            }
        }
    }
}

// ============================================================================
// Thread-local heap access
// ============================================================================

/// Thread-local heap wrapper that owns the heap and its control block.
pub struct ThreadLocalHeap {
    /// The thread's control block for GC coordination.
    pub tcb: std::sync::Arc<ThreadControlBlock>,
}

impl ThreadLocalHeap {
    fn new() -> Self {
        let thread_id = {
            let registry = thread_registry().lock().unwrap();
            registry.threads.len()
        };
        let tcb = std::sync::Arc::new(ThreadControlBlock::new(thread_id));
        {
            let mut registry = thread_registry().lock().unwrap();

            // CRITICAL FIX: Handle thread spawning during GC
            // If GC is already in progress, we must NOT participate in rendezvous.
            // Otherwise:
            // 1. Collector takes snapshot of threads before we register
            // 2. We register and enter rendezvous, storing our roots
            // 3. Collector never sees our roots (snapshot doesn't include us)
            // 4. Collector sweeps objects reachable from our stack → use-after-free
            //
            // NOTE: We check the global gc_in_progress flag instead of thread-local
            // is_collecting(), because a newly spawned thread always sees its own copy
            // of the thread-local variable (default: false), even when collector's copy
            // is true. The global flag correctly reflects the actual GC state.
            if registry.is_gc_in_progress() {
                // GC is in progress - DO NOT set gc_requested flag
                // Thread will run and allocate during GC, but won't enter rendezvous
                // This is safe because:
                // - Thread only allocates NEW objects (not reachable yet)
                // - Old objects from other heaps are already marked
                // - New objects won't be swept (GC already took snapshot of threads)
            } else if GC_REQUESTED.load(Ordering::Acquire) {
                // GC has been requested but not yet started
                // Set gc_requested so we'll participate in handshake when it starts
                tcb.gc_requested.store(true, Ordering::Release);
            }

            registry.register_thread(tcb.clone());
            registry.active_count.fetch_add(1, Ordering::SeqCst);
        }
        Self { tcb }
    }
}

impl Drop for ThreadLocalHeap {
    fn drop(&mut self) {
        let mut registry = thread_registry().lock().unwrap();
        if self.tcb.state.load(Ordering::SeqCst) == THREAD_STATE_EXECUTING {
            registry.active_count.fetch_sub(1, Ordering::SeqCst);
        }
        registry.unregister_thread(&self.tcb);
    }
}

thread_local! {
    /// Thread-local heap instance with its control block.
    pub static HEAP: ThreadLocalHeap = ThreadLocalHeap::new();
}

/// Execute a function with access to the thread-local heap.
pub fn with_heap<F, R>(f: F) -> R
where
    F: FnOnce(&mut LocalHeap) -> R,
{
    HEAP.with(|local| unsafe { f(&mut *local.tcb.heap.get()) })
}

/// Get mutable access to the thread-local heap and its control block.
/// Used for GC coordination.
#[allow(dead_code)]
pub fn with_heap_and_tcb<F, R>(f: F) -> R
where
    F: FnOnce(&mut LocalHeap, &ThreadControlBlock) -> R,
{
    HEAP.with(|local| unsafe { f(&mut *local.tcb.heap.get(), &local.tcb) })
}

/// Get the current thread's control block.
/// Returns None if called outside a thread with GC heap.
#[allow(dead_code)]
#[must_use]
pub fn current_thread_control_block() -> Option<std::sync::Arc<ThreadControlBlock>> {
    HEAP.try_with(|local| local.tcb.clone()).ok()
}

/// Update the heap pointer in the thread control block.
/// Called after heap operations that might move/reallocate heap metadata.
#[allow(dead_code)]
pub const fn update_tcb_heap_ptr() {
    // No-op now since heap is stored directly in TCB
}

/// Get the minimum address managed by the thread-local heap.
#[must_use]
pub fn heap_start() -> usize {
    HEAP.with(|h| unsafe { (*h.tcb.heap.get()).min_addr })
}

/// Get the maximum address managed by the thread-local heap.
#[must_use]
pub fn heap_end() -> usize {
    HEAP.with(|h| unsafe { (*h.tcb.heap.get()).max_addr })
}

/// Convert a pointer to its page header.
///
/// # Safety
/// The pointer must be within a valid GC page.
#[must_use]
pub unsafe fn ptr_to_page_header(ptr: *const u8) -> NonNull<PageHeader> {
    let page_addr = (ptr as usize) & PAGE_MASK;
    // SAFETY: Caller guarantees ptr is within a valid GC page.
    unsafe { NonNull::new_unchecked(page_addr as *mut PageHeader) }
}

/// Get the owner thread ID of the page containing a pointer.
///
/// Returns the thread ID of the thread that owns this page.
/// This is used by parallel GC for Remote Mentions.
///
/// # Safety
/// The pointer must be within a valid GC page.
#[must_use]
pub unsafe fn ptr_to_page_owner(ptr: *const u8) -> usize {
    // SAFETY: Caller guarantees ptr is within a valid GC page.
    unsafe {
        let header = ptr_to_page_header(ptr);
        (*header.as_ptr()).owner_id
    }
}

// 2-arg ptr_to_object_index removed

/// Calculate the object index for a pointer within a page.
///
/// # Safety
/// The pointer must be valid and point within a GC page.
#[allow(dead_code)]
#[must_use]
pub unsafe fn ptr_to_object_index(ptr: *const u8) -> Option<usize> {
    // SAFETY: Caller guarantees ptr is valid
    unsafe {
        let header = ptr_to_page_header(ptr);
        if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
            return None;
        }

        let block_size = (*header.as_ptr()).block_size as usize;
        let header_size = PageHeader::header_size(block_size);
        let page_addr = header.as_ptr() as usize;
        let ptr_addr = ptr as usize;

        if ptr_addr < page_addr + header_size {
            return None;
        }

        let offset = ptr_addr - (page_addr + header_size);
        let index = offset / block_size;

        if index >= (*header.as_ptr()).obj_count as usize {
            return None;
        }

        Some(index)
    }
}

// ============================================================================
// Pointer utilities for BiBOP
// ============================================================================

// Removed duplicate definitions of ptr_to_page_header, is_gc_pointer, ptr_to_object_index
// (The new NonNull versions are defined above)

/// Validate that a pointer is within a GC-managed page.
///
/// # Safety
///
/// The pointer must be valid for reading.
#[allow(dead_code)]
#[must_use]
pub unsafe fn is_gc_pointer(ptr: *const u8) -> bool {
    // SAFETY: Caller guarantees ptr is valid
    unsafe {
        let header = ptr_to_page_header(ptr);
        // header is NonNull. We assume address is accessible as per safety doc.
        (*header.as_ptr()).magic == MAGIC_GC_PAGE
    }
}

/// Try to find a valid GC object starting address from a potential interior pointer.
///
/// This is the core of conservative stack scanning. It takes a potential pointer
/// and, if it points into the GC heap, returns the address of the start of the
/// containing `GcBox`.
///
/// # Safety
///
/// The pointer must be safe to read if it is a valid pointer.
#[allow(dead_code)]
#[must_use]
pub unsafe fn find_gc_box_from_ptr(
    heap: &LocalHeap,
    ptr: *const u8,
) -> Option<NonNull<crate::ptr::GcBox<()>>> {
    let addr = ptr as usize;
    // 1. Quick range check
    if !heap.is_in_range(addr) {
        return None;
    }

    // 2. Check if the pointer is aligned to something that could be a pointer
    unsafe {
        if addr % std::mem::align_of::<usize>() != 0 {
            return None;
        }

        // 3. Check large object map first (handles multi-page objects and avoids reading uninit tail pages)
        let page_addr = addr & crate::heap::PAGE_MASK;
        let (header_ptr_to_use, block_size_to_use, header_size_to_use, offset_to_use) =
            if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
                let h_ptr = head_addr as *mut PageHeader;

                // Recover provenance for Miri
                #[cfg(miri)]
                let h_ptr = heap
                    .large_object_pages()
                    .iter() // Assuming large_object_pages returns Vec<NonNull>
                    .find(|p| p.as_ptr() as usize == head_addr)
                    .map_or(h_ptr, |p| p.as_ptr());

                if addr < head_addr + h_size {
                    return None;
                }
                (h_ptr, size, h_size, addr - (head_addr + h_size))
            } else {
                // Not in large object map, must be small object page with header
                #[allow(unused_mut)]
                let mut header_ptr = ptr_to_page_header(ptr).as_ptr();

                // Recover provenance for Miri
                #[cfg(miri)]
                {
                    header_ptr = heap
                        .all_pages()
                        .find(|p| p.as_ptr() as usize == (addr & crate::heap::PAGE_MASK))
                        .map_or(header_ptr, |p| p.as_ptr());
                }

                // SAFETY CHECK: Is this page actually managed by us?
                // Before reading magic, verify it's in our pages list.
                // This avoids SIGSEGV on gaps in address space between pages.
                if !heap.small_pages.contains(&(addr & crate::heap::PAGE_MASK)) {
                    return None;
                }

                if (*header_ptr).magic == MAGIC_GC_PAGE {
                    let header = &*header_ptr;
                    let b_size = header.block_size as usize;
                    let h_size = PageHeader::header_size(b_size);

                    if addr < (header_ptr as usize) + h_size {
                        return None;
                    }
                    (
                        header_ptr,
                        b_size,
                        h_size,
                        addr - ((header_ptr as usize) + h_size),
                    )
                } else {
                    return None;
                }
            };

        let header = &*header_ptr_to_use;
        let index = offset_to_use / block_size_to_use;

        // 5. Index check
        if index >= header.obj_count as usize {
            return None;
        }

        // 6. Large object handling: with the map, we now support interior pointers!
        // For large objects, we ensure the pointer is within the allocated bounds.
        if header.flags & 0x01 != 0 {
            if offset_to_use >= block_size_to_use {
                return None;
            }
        } else if offset_to_use % block_size_to_use != 0 {
            // For small objects, we still require them to point to the start of an object
            // unless we want to support interior pointers for small objects too.
            // Currently, only large objects (which often contain large buffers)
            // really need interior pointer support for things like array slicing.
            return None;
        }

        // Bingo! We found a potential object.
        let obj_ptr = header_ptr_to_use
            .cast::<u8>()
            .wrapping_add(header_size_to_use)
            .wrapping_add(index * block_size_to_use);
        #[allow(clippy::cast_ptr_alignment)]
        Some(NonNull::new_unchecked(
            obj_ptr.cast::<crate::ptr::GcBox<()>>(),
        ))
    }
}
