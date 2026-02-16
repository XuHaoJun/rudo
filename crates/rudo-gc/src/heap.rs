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
use std::sync::Arc;

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};

use sys_alloc::{Mmap, MmapOptions};

use crate::handles::{AsyncScopeData, AsyncScopeEntry, LocalHandles};
use crate::ptr::GcBox;

/// Global SATB buffer for cross-thread mutations.
/// When a mutation occurs on a different thread than the allocating thread,
/// the SATB old value is recorded here instead of the thread-local buffer.
static CROSS_THREAD_SATB_BUFFER: parking_lot::Mutex<Vec<usize>> =
    parking_lot::Mutex::new(Vec::new());

// Thread-local storage for the current thread's stable ID.
// Assigned once when the thread first accesses the heap.
// IDs start at 1; 0 is reserved as a sentinel for "no associated thread"
// (e.g., objects outside the GC heap or deallocated objects).
thread_local! {
    static THREAD_STABLE_ID: u64 = {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
}

/// Get a unique identifier for the current thread.
/// Uses a stable ID assigned when the thread first accesses the heap.
/// This is different from the stack-address-based ID used for page ownership.
#[must_use]
#[allow(clippy::ptr_as_ptr, clippy::unnecessary_cast)]
pub(crate) fn get_thread_id() -> u64 {
    THREAD_STABLE_ID.with(|id| *id)
}

/// Get the allocating thread's stable ID (u64) for a `GcBox` address.
/// Uses the page header's `owner_thread` field instead of the global `HashMap`.
/// Returns 0 if the address is not in the GC heap.
///
/// Note: 0 is a sentinel value meaning "no associated thread".
/// This occurs for objects outside the GC heap or deallocated objects.
///
/// # Safety
/// - `gc_box_addr` must point to a valid `GcBox` in the heap
/// - The `GcBox` must not have been deallocated
#[must_use]
pub(crate) unsafe fn get_allocating_thread_id(gc_box_addr: usize) -> u64 {
    let heap_start = heap_start();
    let heap_end = heap_end();

    debug_assert!(
        gc_box_addr >= heap_start && gc_box_addr <= heap_end,
        "Address {gc_box_addr:#x} outside heap range [{heap_start:#x}, {heap_end:#x}]"
    );

    if gc_box_addr < heap_start || gc_box_addr > heap_end {
        return 0;
    }

    let header = unsafe { ptr_to_page_header(gc_box_addr as *const u8) };

    if let Some(idx) = unsafe { ptr_to_object_index(gc_box_addr as *const u8) } {
        debug_assert!(
            unsafe { (*header.as_ptr()).is_allocated(idx) },
            "Reading owner_thread from potentially free'd object at index {idx}"
        );
        debug_assert!(
            idx < usize::from(unsafe { (*header.as_ptr()).obj_count }),
            "Object index {idx} exceeds object count {}",
            unsafe { (*header.as_ptr()).obj_count }
        );
    }

    unsafe { (*header.as_ptr()).owner_thread }
}

// ============================================================================
// Cross-Thread Handle Root Storage
// ============================================================================

/// Root entries for cross-thread handles. Protected by Mutex so that
/// handles can be registered/unregistered from any thread.
///
/// Lock ordering: This mutex is acquired AFTER `LocalHeap`, `GlobalMarkState`,
/// and `GcRequest` locks to prevent deadlocks.
pub(crate) struct CrossThreadRootTable {
    /// Monotonically increasing ID counter.
    next_id: u64,
    /// Strong handle root entries: maps `HandleId` -> raw `GcBox` pointer.
    /// These are treated as roots during GC marking.
    pub(crate) strong: HashMap<HandleId, NonNull<GcBox<()>>>,
}

impl CrossThreadRootTable {
    /// Create a new empty root table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: 0,
            strong: HashMap::new(),
        }
    }

    /// Allocate a new unique handle ID.
    #[must_use]
    #[allow(dead_code, clippy::missing_const_for_fn)]
    pub fn allocate_id(&mut self) -> HandleId {
        let id = HandleId(self.next_id);
        self.next_id += 1;
        id
    }
}

/// Opaque ID for a registered cross-thread handle root entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HandleId(pub(crate) u64);

impl HandleId {
    /// Sentinel value indicating the handle is not registered.
    #[allow(dead_code)]
    #[allow(clippy::use_self)]
    pub(crate) const INVALID: HandleId = Self(u64::MAX);
}

// ============================================================================
// Thread Registry & Control Block - Multi-threaded GC Support
// ============================================================================

/// Thread state: executing mutator code.
pub const THREAD_STATE_EXECUTING: usize = 0;
/// Thread state: at a safe point, waiting for GC.
pub const THREAD_STATE_SAFEPOINT: usize = 1;
/// Thread state: inactive (blocked in syscall).
pub const THREAD_STATE_INACTIVE: usize = 2;

/// Shared control block for each thread's GC coordination.
pub struct ThreadControlBlock {
    /// Atomic state of the thread (EXECUTING, SAFEPOINT, or INACTIVE).
    pub state: AtomicUsize,
    /// Flag set by the collector to request a handshake.
    pub gc_requested: AtomicBool,
    /// Condition variable to park the thread during GC.
    pub park_cond: Condvar,
    /// Mutex protecting the condition variable.
    pub park_mutex: Mutex<()>,
    /// The thread's `LocalHeap`.
    pub heap: UnsafeCell<LocalHeap>,
    /// Stack roots captured at safepoint for the collector to scan.
    pub stack_roots: Mutex<Vec<*const u8>>,
    /// Local handles for `HandleScope` v2 support.
    local_handles: UnsafeCell<LocalHandles>,
    /// Async scope registry for cross-await handle tracking.
    async_scopes: Mutex<Vec<AsyncScopeEntry>>,
    /// Set of active async scope IDs for O(1) scope validity checking.
    /// Used by `AsyncHandle::get()` to detect use-after-free.
    active_scope_ids: Mutex<HashSet<u64>>,
    /// Local work queue for incremental marking.
    /// Reduces contention on global worklist.
    local_mark_queue: Vec<NonNull<GcBox<()>>>,
    /// Number of objects this thread marked this slice.
    marked_this_slice: usize,
    /// Per-thread remembered buffer for write barrier batching.
    /// Holds dirty pages before flushing to global list.
    remembered_buffer: Vec<NonNull<PageHeader>>,
    /// Capacity of the remembered buffer.
    #[allow(dead_code)]
    remembered_buffer_capacity: usize,
    /// Controls whether this thread's work can be stolen by other workers.
    /// When true (default), normal work-stealing behavior applies.
    /// When false, this thread's work is only processed by itself.
    stealing_allowed: bool,
    /// Root entries for cross-thread handles. Protected by Mutex so that
    /// handles can be registered/unregistered from any thread.
    ///
    /// Lock ordering: `LocalHeap` → `GlobalMarkState` → `GcRequest` → `CrossThreadRootTable`
    pub(crate) cross_thread_roots: Mutex<CrossThreadRootTable>,
}

#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for ThreadControlBlock {}
unsafe impl Sync for ThreadControlBlock {}

impl Default for ThreadControlBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadControlBlock {
    /// Create a new `ThreadControlBlock` with an uninitialized heap.
    /// The heap must be initialized separately.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: AtomicUsize::new(THREAD_STATE_EXECUTING),
            gc_requested: AtomicBool::new(false),
            park_cond: Condvar::new(),
            park_mutex: Mutex::new(()),
            heap: UnsafeCell::new(LocalHeap::new()),
            stack_roots: Mutex::new(Vec::new()),
            local_handles: UnsafeCell::new(LocalHandles::new()),
            async_scopes: Mutex::new(Vec::new()),
            active_scope_ids: Mutex::new(HashSet::new()),
            local_mark_queue: Vec::new(),
            marked_this_slice: 0,
            remembered_buffer: Vec::with_capacity(32),
            remembered_buffer_capacity: 32,
            stealing_allowed: true,
            cross_thread_roots: Mutex::new(CrossThreadRootTable::new()),
        }
    }

    /// Get a mutable reference to the heap.
    pub fn heap_mut(&mut self) -> &mut LocalHeap {
        unsafe { &mut *self.heap.get() }
    }

    /// Get an immutable reference to the heap.
    pub fn heap(&self) -> &LocalHeap {
        unsafe { &*self.heap.get() }
    }

    /// Get a raw pointer to local handles for interior mutability.
    #[inline]
    #[allow(clippy::missing_const_for_fn)]
    pub fn local_handles_ptr(&self) -> *mut LocalHandles {
        self.local_handles.get()
    }

    /// Get a mutable reference to local handles.
    ///
    /// # Safety
    ///
    /// Caller must ensure exclusive access.
    pub fn local_handles_mut(&mut self) -> &mut LocalHandles {
        unsafe { &mut *self.local_handles.get() }
    }

    /// Register an async scope for GC root tracking.
    ///
    /// Takes OWNERSHIP of an `Arc<AsyncScopeData>`.
    /// The TCB will hold this Arc for the scope's lifetime.
    ///
    /// # Safety Notes
    ///
    /// The caller must NOT access the data after registration if they
    /// plan to drop their Arc - use `Arc::clone()` if continued access needed.
    /// Both the caller and TCB hold independent Arc references.
    #[allow(clippy::missing_panics_doc)]
    pub fn register_async_scope(&self, id: u64, data: Arc<AsyncScopeData>) {
        let entry = AsyncScopeEntry { id, data };
        self.async_scopes.lock().unwrap().push(entry);
        self.active_scope_ids.lock().unwrap().insert(id);
    }

    /// Unregister an async scope.
    #[allow(clippy::missing_panics_doc)]
    pub fn unregister_async_scope(&self, id: u64) {
        self.async_scopes.lock().unwrap().retain(|e| e.id != id);
        self.active_scope_ids.lock().unwrap().remove(&id);
    }

    /// Check if an async scope is still active.
    ///
    /// This is used by `AsyncHandle::get()` to verify the scope hasn't been dropped.
    ///
    /// # Returns
    ///
    /// `true` if the scope ID is still active, `false` otherwise.
    #[inline]
    pub fn is_scope_active(&self, id: u64) -> bool {
        self.active_scope_ids.lock().unwrap().contains(&id)
    }

    /// Iterate all handles (sync and async) as GC roots.
    #[allow(clippy::missing_panics_doc)]
    pub fn iterate_all_handles<F>(&self, mut visitor: F)
    where
        F: FnMut(*const crate::ptr::GcBox<()>),
    {
        // Iterate sync handles
        unsafe {
            (*self.local_handles.get()).iterate(&mut visitor);
        }

        // Iterate async handles
        let scopes = self.async_scopes.lock().unwrap();
        for entry in scopes.iter() {
            unsafe {
                let used = (*entry.data.used.get()).load(Ordering::Acquire);
                let slots = &*entry.data.block.slots.get();
                for slot in slots.iter().take(used) {
                    if !slot.is_null() {
                        visitor(slot.as_ptr());
                    }
                }
            }
        }
    }

    /// Iterate cross-thread handle roots for GC marking.
    ///
    /// This is called during the mark phase to ensure objects referenced
    /// by cross-thread handles are kept alive.
    ///
    /// # Safety
    ///
    /// The caller must hold any locks required by the lock ordering discipline.
    /// This method acquires `cross_thread_roots` lock.
    #[allow(clippy::missing_panics_doc)]
    pub(crate) fn iterate_cross_thread_roots<F>(&self, mut visitor: F)
    where
        F: FnMut(*const crate::ptr::GcBox<()>),
    {
        let roots = self.cross_thread_roots.lock().unwrap();
        for ptr in roots.strong.values() {
            // SAFETY: ptr validity is guaranteed because the handle registered
            // it before releasing the lock, and the GC holds the lock now,
            // so no concurrent Drop can remove it mid-iteration.
            visitor(ptr.as_ptr());
        }
    }

    // =========================================================================
    // Incremental Marking Support
    // =========================================================================

    /// Push work to thread-local mark queue.
    /// Overflows to global worklist when local queue is full.
    pub fn push_local_mark_work(&mut self, ptr: NonNull<GcBox<()>>) {
        self.local_mark_queue.push(ptr);
    }

    /// Pop work from thread-local mark queue.
    /// Tries to steal from global worklist if local is empty.
    pub fn pop_local_mark_work(&mut self) -> Option<NonNull<GcBox<()>>> {
        self.local_mark_queue.pop()
    }

    /// Get reference to local mark queue for iteration.
    #[allow(clippy::missing_const_for_fn)]
    pub fn local_mark_queue(&self) -> &Vec<NonNull<GcBox<()>>> {
        &self.local_mark_queue
    }

    /// Get mutable reference to local mark queue.
    #[allow(clippy::missing_const_for_fn)]
    pub fn local_mark_queue_mut(&mut self) -> &mut Vec<NonNull<GcBox<()>>> {
        &mut self.local_mark_queue
    }

    /// Get count of objects marked this slice.
    #[allow(clippy::missing_const_for_fn)]
    pub fn marked_this_slice(&self) -> usize {
        self.marked_this_slice
    }

    /// Increment the marked counter.
    #[allow(clippy::missing_const_for_fn)]
    pub fn inc_marked_this_slice(&mut self, count: usize) {
        self.marked_this_slice += count;
    }

    /// Reset slice-local counters.
    pub fn reset_slice_counters(&mut self) {
        self.marked_this_slice = 0;
        self.remembered_buffer.clear();
    }

    /// Check if work-stealing is allowed for this thread.
    #[inline]
    #[allow(clippy::missing_const_for_fn)]
    pub fn stealing_allowed(&self) -> bool {
        self.stealing_allowed
    }

    /// Enable or disable work-stealing for this thread.
    #[inline]
    #[allow(clippy::missing_const_for_fn)]
    pub fn set_stealing_allowed(&mut self, allowed: bool) {
        self.stealing_allowed = allowed;
    }
}

/// Global registry of all threads with GC heaps.
pub struct ThreadRegistry {
    /// All active thread control blocks.
    pub threads: Vec<std::sync::Arc<ThreadControlBlock>>,
    /// Number of threads currently in EXECUTING state.
    pub active_count: AtomicUsize,
    /// Global flag indicating if a GC collection is currently in progress.
    /// This is used to detect if GC is in progress when new threads spawn,
    /// since thread-local `IN_COLLECT` can't be used across threads.
    pub gc_in_progress: AtomicBool,
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
        }
    }

    /// Register a new thread with the registry.
    pub fn register_thread(&mut self, tcb: std::sync::Arc<ThreadControlBlock>) {
        self.threads.push(tcb);
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
}

static THREAD_REGISTRY: OnceLock<Mutex<ThreadRegistry>> = OnceLock::new();

/// Access the global thread registry with lock ordering validation.
///
/// This function wraps the thread registry lock acquisition with validation
/// to ensure proper lock ordering discipline.
///
/// # Lock Ordering
///
/// The thread registry lock has order 2 (`GlobalMarkState`). Callers must
/// not hold any locks with order > 2 when calling this function.
#[inline]
pub fn thread_registry() -> &'static Mutex<ThreadRegistry> {
    // Validate lock ordering: thread_registry is level 2
    // Cannot be called while holding GC Request lock (level 3)
    #[cfg(debug_assertions)]
    {
        use crate::gc::sync::{acquire_lock, get_current_lock_level, LockOrder};
        let current_level = get_current_lock_level();
        acquire_lock(LockOrder::GlobalMarkState, current_level);
    }
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
/// Uses Acquire ordering to ensure we see the complete GC request state.
pub fn check_safepoint() {
    // CRITICAL FIX: Prevent deadlock when Drop handlers allocate during GC
    // If we're already collecting, we must NOT enter rendezvous or we'll
    // deadlock waiting for gc_requested to become false (only collector can clear it)
    if GC_REQUESTED.load(Ordering::Acquire) && !crate::gc::is_collecting() {
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

    let gc_global = GC_REQUESTED.load(Ordering::Acquire);
    let gc_local = tcb.gc_requested.load(Ordering::Acquire);

    if !gc_global && !gc_local {
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

    let mut roots = Vec::new();
    unsafe {
        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
            roots.push(ptr as *const u8);
        });
    }
    *tcb.stack_roots.lock().unwrap() = roots;

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
/// This function acquires the thread registry lock (order 2) to safely
/// access and modify thread state.
///
/// # Lock Ordering
///
/// Acquires `thread_registry()` lock (order 2). Caller must not hold
/// any locks with order > 2.
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

/// Request all threads to stop at the next safe point.
/// Returns true if this thread should become the collector.
///
/// # Panics
///
/// Panics if the thread registry lock is poisoned.
#[allow(dead_code)]
pub fn request_gc_handshake() -> bool {
    let registry = thread_registry().lock().unwrap();

    // Set GC_REQUESTED flag first (before locking registry)
    GC_REQUESTED.store(true, Ordering::Relaxed);

    // Set per-thread gc_requested flag for all threads
    for tcb in &registry.threads {
        tcb.gc_requested.store(true, Ordering::Relaxed);
    }

    let active = registry.active_count.load(Ordering::Acquire);
    drop(registry);

    active == 1
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

/// Size of each memory page. Determined at runtime to support platforms
/// with page sizes other than 4KB (e.g., Windows 64KB allocation granularity).
static PAGE_SIZE: OnceLock<usize> = OnceLock::new();

/// Returns the system page size.
pub fn page_size() -> usize {
    *PAGE_SIZE.get_or_init(sys_alloc::allocation_granularity)
}

/// Mask for extracting page address from a pointer.
#[must_use]
pub fn page_mask() -> usize {
    !(page_size() - 1)
}

/// Target address for heap allocation (Address Space Coloring).
/// We aim for `0x6000_0000_0000` on 64-bit systems.
#[cfg(target_pointer_width = "64")]
pub const HEAP_HINT_ADDRESS: usize = 0x6000_0000_0000;

/// Target address for heap allocation on 32-bit systems.
#[cfg(target_pointer_width = "32")]
pub const HEAP_HINT_ADDRESS: usize = 0x4000_0000;

/// Magic number for validating GC pages ("RUDG" in ASCII).
pub const MAGIC_GC_PAGE: u32 = 0x5255_4447;

/// Flag: Page is a large object.
pub const PAGE_FLAG_LARGE: u8 = 0x01;
/// Flag: Page is an orphan (owner thread has terminated).
pub const PAGE_FLAG_ORPHAN: u8 = 0x02;
/// Flag: Page needs lazy sweep (has dead objects to reclaim).
#[cfg(feature = "lazy-sweep")]
pub const PAGE_FLAG_NEEDS_SWEEP: u8 = 0x04;
/// Flag: All objects in page are dead (fast path for lazy sweep).
#[cfg(feature = "lazy-sweep")]
pub const PAGE_FLAG_ALL_DEAD: u8 = 0x08;
/// Flag: Page is in the dirty pages list (old generation with dirty objects).
pub const PAGE_FLAG_DIRTY_LISTED: u8 = 0x10;

/// Maximum number of u64 words in a bitmap to support 64KB pages with 16-byte blocks.
pub const BITMAP_SIZE: usize = 64;

/// Size classes for object allocation.
/// Objects are routed to the smallest size class that fits them.
#[allow(dead_code)]
pub const SIZE_CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// Objects larger than this go to the Large Object Space.
pub const MAX_SMALL_OBJECT_SIZE: usize = 2048;

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
    pub flags: AtomicU8,
    /// Thread ID of the owner thread (for work-stealing).
    pub owner_thread: u64,
    /// Count of dead objects in this page (for "all-dead" fast path).
    #[cfg(feature = "lazy-sweep")]
    pub dead_count: AtomicU16,
    /// Padding for alignment (used for dead_count when lazy-sweep is disabled).
    #[cfg(not(feature = "lazy-sweep"))]
    _padding: [u8; 2],
    /// Bitmap of marked objects (atomic for concurrent marking).
    pub mark_bitmap: [AtomicU64; BITMAP_SIZE],
    /// Bitmap of dirty objects (atomic for concurrent write barriers).
    pub dirty_bitmap: [AtomicU64; BITMAP_SIZE],
    /// Bitmap of allocated objects (atomic for proper synchronization during sweep).
    pub allocated_bitmap: [AtomicU64; BITMAP_SIZE],
    /// Index of first free slot in free list (atomic for concurrent access).
    #[cfg(feature = "lazy-sweep")]
    pub free_list_head: AtomicU16,
    /// Index of first free slot in free list (non-atomic, single-threaded).
    #[cfg(not(feature = "lazy-sweep"))]
    pub free_list_head: u16,
}

impl PageHeader {
    #[cfg(feature = "lazy-sweep")]
    const FREE_LIST_NONE: u16 = u16::MAX;

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
    pub fn max_objects(block_size: usize) -> usize {
        (page_size() - Self::header_size(block_size)) / block_size
    }

    /// Check if an object at the given index is marked.
    #[must_use]
    pub fn is_marked(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.mark_bitmap[word].load(Ordering::Acquire) & (1 << bit)) != 0
    }

    /// Set the mark bit for an object (atomic, suitable for concurrent marking).
    /// Returns true if the bit was newly set (not previously marked).
    pub fn set_mark(&mut self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        let mask = 1u64 << bit;
        let old = self.mark_bitmap[word].fetch_or(mask, Ordering::AcqRel);
        (old & mask) == 0
    }

    /// Try to mark an object atomically using CAS.
    /// Returns `Ok(true)` if this thread marked it, `Ok(false)` if already marked.
    ///
    /// Returns `Err(())` if the CAS failed due to concurrent modification,
    /// which means another thread modified the word concurrently.
    ///
    /// This is used for parallel marking where multiple threads may attempt
    /// to mark the same object concurrently.
    ///
    /// # Errors
    ///
    /// Returns `Err(())` if a concurrent modification was detected during the CAS.
    #[allow(clippy::result_unit_err)]
    pub fn try_mark(&self, index: usize) -> Result<bool, ()> {
        let word = index / 64;
        let bit = index % 64;
        let mask = 1u64 << bit;
        let old = self.mark_bitmap[word].load(Ordering::Acquire);
        if (old & mask) != 0 {
            return Ok(false);
        }
        match self.mark_bitmap[word].compare_exchange(
            old,
            old | mask,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(true),
            Err(_) => Err(()),
        }
    }

    /// Check if all allocated objects in this page are marked.
    /// This is used to determine if a page is fully processed during marking.
    #[must_use]
    pub fn is_fully_marked(&self) -> bool {
        for word_idx in 0..BITMAP_SIZE {
            let mark_word = self.mark_bitmap[word_idx].load(Ordering::Acquire);
            let alloc_word = self.allocated_bitmap[word_idx].load(Ordering::Acquire);
            if (mark_word & alloc_word) != alloc_word {
                return false;
            }
        }
        true
    }

    /// Clear the mark bit for an object at the given index.
    #[allow(dead_code)]
    pub fn clear_mark(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.mark_bitmap[word].fetch_and(!(1u64 << bit), Ordering::Release);
    }

    /// Clear all mark bits.
    pub fn clear_all_marks(&mut self) {
        for word in &self.mark_bitmap {
            word.store(0u64, Ordering::Release);
        }
    }

    /// Check if an object at the given index is dirty.
    #[must_use]
    pub fn is_dirty(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.dirty_bitmap[word].load(Ordering::Acquire) & (1 << bit)) != 0
    }

    /// Set the dirty bit for an object at the given index.
    pub fn set_dirty(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word].fetch_or(1u64 << bit, Ordering::AcqRel);
    }

    /// Clear the dirty bit for an object at the given index.
    #[allow(dead_code)]
    pub fn clear_dirty(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word].fetch_and(!(1u64 << bit), Ordering::Release);
    }

    /// Clear all dirty bits.
    pub fn clear_all_dirty(&mut self) {
        for word in &self.dirty_bitmap {
            word.store(0u64, Ordering::Release);
        }
    }

    /// Check if page is in the dirty pages list.
    ///
    /// # Memory Ordering
    /// Uses Acquire ordering to synchronize with set operations.
    #[inline]
    #[must_use]
    pub fn is_dirty_listed(&self) -> bool {
        (self.flags.load(Ordering::Acquire) & PAGE_FLAG_DIRTY_LISTED) != 0
    }

    /// Set the dirty-listed flag (called under mutex).
    ///
    /// # Memory Ordering
    /// Uses Release ordering to publish Vec push.
    #[inline]
    pub fn set_dirty_listed(&self) {
        self.flags
            .fetch_or(PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
    }

    /// Clear the dirty-listed flag (called during GC scan).
    ///
    /// # Memory Ordering
    /// Uses Release ordering to publish for next cycle.
    #[inline]
    pub fn clear_dirty_listed(&self) {
        self.flags
            .fetch_and(!PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
    }

    /// Check if an object at the given index is allocated.
    #[must_use]
    pub fn is_allocated(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.allocated_bitmap[word].load(Ordering::Acquire) & (1 << bit)) != 0
    }

    /// Set the allocated bit for an object at the given index.
    pub fn set_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word].fetch_or(1u64 << bit, Ordering::AcqRel);
    }

    /// Clear the allocated bit for an object at the given index.
    pub fn clear_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word].fetch_and(!(1u64 << bit), Ordering::Release);
    }

    /// Clear all allocated bits.
    pub fn clear_all_allocated(&mut self) {
        for word in &self.allocated_bitmap {
            word.store(0u64, Ordering::Release);
        }
    }

    #[cfg(feature = "lazy-sweep")]
    /// Check if page needs lazy sweep.
    /// Uses Acquire ordering to synchronize with `dead_count` reads.
    pub fn needs_sweep(&self) -> bool {
        (self.flags.load(Ordering::Acquire) & PAGE_FLAG_NEEDS_SWEEP) != 0
    }

    #[cfg(feature = "lazy-sweep")]
    /// Set the `needs_sweep` flag.
    /// Uses Release ordering to ensure prior writes are visible to readers.
    pub fn set_needs_sweep(&self) {
        self.flags
            .fetch_or(PAGE_FLAG_NEEDS_SWEEP, Ordering::Release);
    }

    #[cfg(feature = "lazy-sweep")]
    /// Clear the `needs_sweep` flag.
    /// Uses Release ordering. Callers must ensure proper synchronization
    /// (e.g., atomic fence) before this if synchronizing with `dead_count` reads.
    pub fn clear_needs_sweep(&self) {
        self.flags
            .fetch_and(!PAGE_FLAG_NEEDS_SWEEP, Ordering::Release);
    }

    #[cfg(feature = "lazy-sweep")]
    /// Check if all objects in page are dead.
    /// Uses Acquire ordering for consistency with other lazy-sweep operations.
    pub fn all_dead(&self) -> bool {
        (self.flags.load(Ordering::Acquire) & PAGE_FLAG_ALL_DEAD) != 0
    }

    #[cfg(feature = "lazy-sweep")]
    /// Set the `all_dead` flag.
    /// Uses Release ordering to ensure prior writes are visible.
    pub fn set_all_dead(&self) {
        self.flags.fetch_or(PAGE_FLAG_ALL_DEAD, Ordering::Release);
    }

    #[cfg(feature = "lazy-sweep")]
    /// Clear the `all_dead` flag.
    /// Uses Release ordering.
    pub fn clear_all_dead(&self) {
        self.flags.fetch_and(!PAGE_FLAG_ALL_DEAD, Ordering::Release);
    }

    /// Get the raw flags value (for internal use).
    /// Uses Relaxed ordering since `PAGE_FLAG_LARGE` is set once at creation
    /// and `PAGE_FLAG_ORPHAN` is set during controlled cleanup.
    pub fn flags(&self) -> u8 {
        self.flags.load(Ordering::Relaxed)
    }

    /// Check if this is a large object page.
    /// Uses Relaxed ordering since `PAGE_FLAG_LARGE` is set once at creation
    /// and `PAGE_FLAG_ORPHAN` is set during controlled cleanup.
    pub fn is_large_object(&self) -> bool {
        (self.flags() & PAGE_FLAG_LARGE) != 0
    }

    /// Set the large object flag.
    /// Uses Relaxed ordering since this is set once at page creation.
    pub fn set_large_object(&self) {
        self.flags.fetch_or(PAGE_FLAG_LARGE, Ordering::Relaxed);
    }

    /// Check if this page is orphaned (waiting to be freed).
    /// Uses Relaxed ordering for single-threaded cleanup operations.
    pub fn is_orphan(&self) -> bool {
        (self.flags() & PAGE_FLAG_ORPHAN) != 0
    }

    /// Set the orphan flag.
    /// Uses Relaxed ordering for single-threaded cleanup operations.
    pub fn set_orphan(&self) {
        self.flags.fetch_or(PAGE_FLAG_ORPHAN, Ordering::Relaxed);
    }

    #[cfg(feature = "lazy-sweep")]
    /// Get the `dead_count`.
    #[allow(clippy::missing_const_for_fn)]
    pub fn dead_count(&self) -> u16 {
        self.dead_count.load(Ordering::Acquire)
    }

    #[cfg(feature = "lazy-sweep")]
    /// Set the `dead_count`.
    #[allow(clippy::missing_const_for_fn)]
    pub fn set_dead_count(&self, count: u16) {
        self.dead_count.store(count, Ordering::Release);
    }

    #[cfg(feature = "lazy-sweep")]
    /// Increment the `dead_count`.
    #[allow(clippy::missing_const_for_fn)]
    pub fn increment_dead_count(&self) {
        let _ = self.dead_count.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(feature = "lazy-sweep")]
    /// Get the raw free list head value. Use `FREE_LIST_NONE` for empty.
    /// Hot path uses this to avoid Option branch overhead.
    #[allow(clippy::missing_const_for_fn)]
    pub fn free_list_head_raw(&self) -> u16 {
        self.free_list_head.load(Ordering::Acquire)
    }

    #[cfg(feature = "lazy-sweep")]
    /// Get the current free list head, returning `None` if empty.
    #[allow(clippy::missing_const_for_fn)]
    pub fn free_list_head(&self) -> Option<u16> {
        let val = self.free_list_head_raw();
        if val == Self::FREE_LIST_NONE {
            None
        } else {
            Some(val)
        }
    }

    #[cfg(feature = "lazy-sweep")]
    /// Try to update the free list head atomically using CAS.
    ///
    /// Returns `Ok(Some(new))` on success, `Err(actual)` if CAS failed.
    ///
    /// # Errors
    ///
    /// Returns `Err(actual)` where `actual` is the current value of `free_list_head`
    /// if another thread modified the free list concurrently.
    #[allow(clippy::missing_const_for_fn)]
    pub fn compare_exchange_free_list(
        &self,
        current: Option<u16>,
        new: Option<u16>,
    ) -> Result<Option<u16>, Option<u16>> {
        let current_val = current.unwrap_or(Self::FREE_LIST_NONE);
        let new_val = new.unwrap_or(Self::FREE_LIST_NONE);
        self.free_list_head
            .compare_exchange(current_val, new_val, Ordering::AcqRel, Ordering::Acquire)
            .map(|v| {
                if v == Self::FREE_LIST_NONE {
                    None
                } else {
                    Some(v)
                }
            })
            .map_err(|v| {
                if v == Self::FREE_LIST_NONE {
                    None
                } else {
                    Some(v)
                }
            })
    }

    #[cfg(feature = "lazy-sweep")]
    /// Set the free list head directly (for initialization).
    #[allow(clippy::missing_const_for_fn)]
    pub fn set_free_list_head(&self, head: Option<u16>) {
        let val = head.unwrap_or(Self::FREE_LIST_NONE);
        self.free_list_head.store(val, Ordering::Release);
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

/// Map `block_size` (16, 32, ..., 2048) to class index 0..7.
#[must_use]
pub(crate) const fn block_size_to_class_index(block_size: usize) -> usize {
    (block_size.trailing_zeros().saturating_sub(4)) as usize
}

// ============================================================================
// GlobalSegmentManager - Shared memory manager
// ============================================================================

/// Orphan page: a page whose owner thread has terminated but may still contain
/// live objects referenced by other threads.
pub struct OrphanPage {
    /// Address of the orphan page.
    pub addr: usize,
    /// Size of the orphan page.
    pub size: usize,
    /// Whether this is a large object page.
    pub is_large: bool,
    /// Thread ID of the original owner.
    pub original_owner: std::thread::ThreadId,
}

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

    /// Orphan pages keyed by page address for O(1) lookup in `find_gc_box_from_orphan`.
    pub orphan_by_addr: HashMap<usize, OrphanPage>,
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
            orphan_by_addr: HashMap::new(),
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
    /// TLABs for each small size class.
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

    /// Pages for objects larger than 2KB (kept separate for some logic?).
    /// Actually, let's keep `pages` as the unified list for simple sweeping.
    /// But we might want `large_object_pages` ref for specific logic.
    /// Original code had `large_objects` separate.
    /// Let's merge them into `pages` for simplicity, OR keep separate if needed.
    /// Current `sweep` logic iterates all segments then large objects.
    /// Merging them is better for simple iteration.
    /// But large objects have different headers... wait, no, same header structure, distinct flag.
    /// So unified list is fine.

    // We retain `large_objects` separately if we want to quickly identify them without checking flags?
    // Nah, flag check is fast.

    /// Map from page address to its corresponding large object head.
    /// Still useful for interior pointers.
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,

    // Stats
    young_allocated: usize,
    old_allocated: usize,
    min_addr: usize,
    max_addr: usize,
    // Quarantined pages (thread-local cache before pushing to global?)
    // Actually GlobalSegmentManager handles this now.
    // We might keep this if we want to avoid lock contention on "discarding" bad pages?
    // But `allocate_page` is now on Manager.
    // So LocalHeap doesn't strictly need this unless we pass it to Manager to avoid re-locking?
    // Manager has its own.
    // We can remove it from here.
    /// Mutex-protected list of pages with dirty objects (old generation only).
    /// Cleared at the end of each minor GC cycle.
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,

    /// Snapshot for lock-free scanning during GC.
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,

    /// Rolling average for capacity planning.
    avg_dirty_pages: usize,

    /// History of dirty page counts (last 4 cycles).
    dirty_page_history: [usize; 4],

    /// Per-thread remembered buffer for incremental GC write barrier.
    /// Batched page recording to reduce lock contention.
    remembered_buffer: Vec<NonNull<PageHeader>>,
    remembered_buffer_capacity: usize,

    /// Per-thread SATB buffer for capturing old pointer values.
    /// Records old values before they're overwritten during incremental marking.
    satb_old_values: Vec<NonNull<GcBox<()>>>,
    satb_buffer_capacity: usize,

    /// Per-thread overflow buffer for SATB values.
    /// When the main SATB buffer overflows, values are preserved here
    /// until they can be processed during fallback/final mark.
    satb_overflow_buffer: Vec<NonNull<GcBox<()>>>,

    /// Per-size-class cache: last page with free slots for O(1) allocation.
    /// Invalidated when page is exhausted; repopulated from O(N) scan fallback.
    free_list_preferred: [Option<NonNull<PageHeader>>; 8],

    /// Per-size-class page index for O(K) `alloc_from_free_list` slow path.
    /// Only small-object pages; large-object pages are omitted.
    pages_by_class: [Vec<NonNull<PageHeader>>; 8],

    /// Per-size-class list of pages that currently have free slots.
    /// Used for O(P) `alloc_from_free_list` scan instead of O(K) over `pages_by_class`,
    /// where P << K (pages with space vs all pages).
    pub(crate) pages_with_free_slots: [Vec<NonNull<PageHeader>>; 8],

    /// Per-size-class buffer of pre-popped pointers.
    /// Refilled from `alloc_from_free_list` in batches; reduces invocation count.
    pub(crate) free_list_buffer: [Vec<NonNull<u8>>; 8],

    /// Scratch buffer for batch refill; avoids Vec allocation in hot path.
    #[cfg(feature = "lazy-sweep")]
    batch_scratch: [Vec<NonNull<u8>>; 8],

    /// Per-size-class cursor: index into `pages` for next pending-sweep scan.
    /// Resets to 0 when a full cycle finds nothing.
    /// Reserved for future round-robin optimization; currently unused with `pending_sweep_by_class`.
    #[cfg(feature = "lazy-sweep")]
    #[allow(dead_code)]
    pending_sweep_cursor: [usize; 8],

    /// Per-size-class index of pages needing sweep for O(K) `alloc_from_pending_sweep`.
    /// Populated when `set_needs_sweep` is called; pruned lazily during iteration.
    #[cfg(feature = "lazy-sweep")]
    pub(crate) pending_sweep_by_class: [Vec<NonNull<PageHeader>>; 8],
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
            dirty_pages: parking_lot::Mutex::new(Vec::with_capacity(64)),
            dirty_pages_snapshot: Vec::new(),
            avg_dirty_pages: 16,
            dirty_page_history: [16; 4],
            remembered_buffer: Vec::with_capacity(32),
            remembered_buffer_capacity: 32,
            satb_old_values: Vec::with_capacity(32),
            satb_buffer_capacity: 32,
            satb_overflow_buffer: Vec::with_capacity(64),
            free_list_preferred: [None; 8],
            pages_by_class: std::array::from_fn(|_| Vec::new()),
            pages_with_free_slots: std::array::from_fn(|_| Vec::new()),
            free_list_buffer: std::array::from_fn(|_| Vec::with_capacity(8)),
            #[cfg(feature = "lazy-sweep")]
            batch_scratch: std::array::from_fn(|_| Vec::with_capacity(8)),
            #[cfg(feature = "lazy-sweep")]
            pending_sweep_cursor: [0; 8],
            #[cfg(feature = "lazy-sweep")]
            pending_sweep_by_class: std::array::from_fn(|_| Vec::new()),
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

    /// Add a page to the dirty pages list if not already present.
    ///
    /// # Safety
    /// Caller must ensure header points to a valid `PageHeader`.
    ///
    /// # Performance
    /// - O(1) if page already listed (early exit via flag check)
    /// - O(1) + mutex if adding new page
    #[allow(clippy::significant_drop_tightening)]
    #[inline]
    pub unsafe fn add_to_dirty_pages(&self, header: NonNull<PageHeader>) {
        // Fast path: already in list
        // SAFETY: Caller guarantees header is valid
        if unsafe { (*header.as_ptr()).is_dirty_listed() } {
            return;
        }

        // Slow path: acquire lock and double-check
        let mut dirty_pages = self.dirty_pages.lock();
        // SAFETY: Caller guarantees header is valid
        if unsafe { !(*header.as_ptr()).is_dirty_listed() } {
            dirty_pages.push(header);
            // SAFETY: Caller guarantees header is valid
            unsafe { (*header.as_ptr()).set_dirty_listed() };
        }
    }

    /// Take a snapshot of dirty pages for GC scanning.
    ///
    /// # Contract
    /// - Called at the start of minor GC, before scanning
    /// - Moves `dirty_pages` contents to snapshot
    /// - Lock released immediately after snapshot
    ///
    /// # Returns
    /// Number of pages in the snapshot
    pub fn take_dirty_pages_snapshot(&mut self) -> usize {
        let mut dirty_pages = self.dirty_pages.lock();
        let capacity = self.avg_dirty_pages.max(16);
        self.dirty_pages_snapshot = Vec::with_capacity(capacity);
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        drop(dirty_pages);
        self.dirty_pages_snapshot.len()
    }

    /// Get iterator over dirty pages snapshot.
    ///
    /// # Contract
    /// - Must only be called after `take_dirty_pages_snapshot()`
    /// - Must be called before `clear_dirty_pages_snapshot()`
    #[inline]
    pub fn dirty_pages_iter(&self) -> impl Iterator<Item = NonNull<PageHeader>> + '_ {
        self.dirty_pages_snapshot.iter().copied()
    }

    /// Clear the snapshot and update statistics.
    ///
    /// # Contract
    /// - Must be called at the end of minor GC
    /// - Updates rolling average for capacity planning
    pub fn clear_dirty_pages_snapshot(&mut self) {
        let count = self.dirty_pages_snapshot.len();
        self.dirty_page_history.rotate_right(1);
        self.dirty_page_history[0] = count;
        self.avg_dirty_pages = self.dirty_page_history.iter().sum::<usize>() / 4;
        self.dirty_pages_snapshot.clear();
    }

    /// Get count of dirty pages (for debugging/metrics and tests).
    pub fn dirty_pages_count(&self) -> usize {
        self.dirty_pages.lock().len()
    }

    /// Record a page in the remembered buffer for incremental GC.
    /// Flushes to global dirty list on overflow.
    ///
    /// Note: We accept duplicates here for O(1) insert performance.
    /// Duplicates are filtered out during flush to the global dirty list.
    #[inline]
    pub fn record_in_remembered_buffer(&mut self, page: NonNull<PageHeader>) {
        self.remembered_buffer.push(page);
        if self.remembered_buffer.len() >= self.remembered_buffer_capacity {
            self.flush_remembered_buffer();
        }
    }

    /// Flush remembered buffer to global dirty list with deduplication.
    #[inline]
    pub fn flush_remembered_buffer(&mut self) {
        let pages = std::mem::take(&mut self.remembered_buffer);
        if pages.is_empty() {
            return;
        }

        let mut dirty_pages = self.dirty_pages.lock();
        let needed = dirty_pages.len() + pages.len();
        let mut unique_pages: std::collections::HashSet<_> =
            std::collections::HashSet::with_capacity(needed);
        unique_pages.extend(dirty_pages.iter().copied());
        unique_pages.extend(pages);

        dirty_pages.clear();
        dirty_pages.extend(unique_pages);
    }

    /// Get remembered buffer capacity.
    #[allow(clippy::missing_const_for_fn)]
    pub fn remembered_buffer_capacity(&self) -> usize {
        self.remembered_buffer_capacity
    }

    /// Set remembered buffer capacity.
    pub fn set_remembered_buffer_capacity(&mut self, capacity: usize) {
        self.remembered_buffer_capacity = capacity;
        self.remembered_buffer = Vec::with_capacity(capacity);
    }

    /// Clear the remembered buffer.
    pub fn clear_remembered_buffer(&mut self) {
        self.remembered_buffer.clear();
    }

    // ========================================================================
    // SATB Buffer for incremental marking
    // ========================================================================

    /// Record an old pointer value for SATB preservation.
    /// Called during write barrier before a pointer is overwritten.
    ///
    /// Returns `true` if the value was stored successfully, `false` if the buffer
    /// overflowed and fallback was requested.
    pub fn record_satb_old_value(&mut self, gc_box: NonNull<GcBox<()>>) -> bool {
        let current_thread_id = get_thread_id();
        let allocating_thread_id = unsafe { get_allocating_thread_id(gc_box.as_ptr() as usize) };

        if current_thread_id != allocating_thread_id && allocating_thread_id != 0 {
            CROSS_THREAD_SATB_BUFFER
                .lock()
                .push(gc_box.as_ptr() as usize);
            return true;
        }

        self.satb_old_values.push(gc_box);
        if self.satb_old_values.len() >= self.satb_buffer_capacity {
            self.satb_buffer_overflowed()
        } else {
            true
        }
    }

    /// Flush the cross-thread SATB buffer.
    /// Called during GC to process cross-thread mutations.
    #[must_use]
    pub fn flush_cross_thread_satb_buffer() -> Vec<NonNull<GcBox<()>>> {
        let addresses = std::mem::take(&mut *CROSS_THREAD_SATB_BUFFER.lock());
        addresses
            .into_iter()
            .filter_map(|addr| NonNull::new(addr as *mut GcBox<()>))
            .collect()
    }

    /// Push a GC pointer to the cross-thread SATB buffer.
    /// Used when recording SATB old values from threads without a GC heap.
    pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
        CROSS_THREAD_SATB_BUFFER
            .lock()
            .push(gc_ptr.as_ptr() as usize);
    }

    fn satb_buffer_overflowed(&mut self) -> bool {
        self.satb_overflow_buffer.append(&mut self.satb_old_values);
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        false
    }

    /// Flush the SATB buffer, returning captured old values.
    /// The caller is responsible for marking these objects.
    #[must_use]
    pub fn flush_satb_buffer(&mut self) -> Vec<NonNull<GcBox<()>>> {
        std::mem::take(&mut self.satb_old_values)
    }

    /// Clear the SATB buffer without processing.
    pub fn clear_satb_buffer(&mut self) {
        self.satb_old_values.clear();
    }

    /// Flush the SATB overflow buffer, returning captured old values.
    /// Called during final mark to process overflowed SATB values.
    #[must_use]
    pub fn flush_satb_overflow_buffer(&mut self) -> Vec<NonNull<GcBox<()>>> {
        std::mem::take(&mut self.satb_overflow_buffer)
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
        const BATCH: usize = 8;
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
            self.update_range(ptr.as_ptr() as usize & page_mask(), page_size());
            return ptr;
        }

        // Serve from buffer if non-empty
        if let Some(ptr) = self.free_list_buffer[class_index].pop() {
            self.update_range(ptr.as_ptr() as usize & page_mask(), page_size());
            return ptr;
        }

        #[cfg(feature = "lazy-sweep")]
        {
            let mut scratch = std::mem::take(&mut self.batch_scratch[class_index]);
            scratch.clear();
            if self.alloc_from_free_list_batch(class_index, &mut scratch, BATCH) {
                let ptr = scratch.remove(0);
                self.free_list_buffer[class_index].append(&mut scratch);
                self.batch_scratch[class_index] = scratch;
                self.update_range(ptr.as_ptr() as usize & page_mask(), page_size());
                return ptr;
            }
            self.batch_scratch[class_index] = scratch;
        }

        // Fallback: single-pop loop
        if let Some(ptr) = self.alloc_from_free_list(class_index) {
            #[cfg(feature = "lazy-sweep")]
            {
                let mut extra = std::mem::take(&mut self.batch_scratch[class_index]);
                extra.clear();
                if !self.alloc_from_free_list_batch(class_index, &mut extra, 7) {
                    for _ in 1..BATCH {
                        if let Some(p) = self.alloc_from_free_list(class_index) {
                            extra.push(p);
                        } else {
                            break;
                        }
                    }
                }
                self.free_list_buffer[class_index].append(&mut extra);
                self.batch_scratch[class_index] = extra;
            }
            #[cfg(not(feature = "lazy-sweep"))]
            for _ in 1..BATCH {
                if let Some(p) = self.alloc_from_free_list(class_index) {
                    self.free_list_buffer[class_index].push(p);
                } else {
                    break;
                }
            }
            self.update_range(ptr.as_ptr() as usize & page_mask(), page_size());
            return ptr;
        }

        #[cfg(feature = "lazy-sweep")]
        if let Some(ptr) = self.alloc_from_pending_sweep(class_index) {
            self.update_range(ptr.as_ptr() as usize & page_mask(), page_size());
            return ptr;
        }

        let ptr = self.alloc_slow(size, class_index);

        self.update_range(ptr.as_ptr() as usize & page_mask(), page_size());
        ptr
    }

    #[cfg(feature = "lazy-sweep")]
    fn alloc_from_pending_sweep(&mut self, class_index: usize) -> Option<NonNull<u8>> {
        if crate::gc::sync::GC_MARK_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }

        let block_size = SIZE_CLASSES[class_index];
        let mut i = 0;
        while i < self.pending_sweep_by_class[class_index].len() {
            let page_ptr = self.pending_sweep_by_class[class_index][i];
            let matches = unsafe {
                let header = page_ptr.as_ptr();
                let hdr = header.read();
                !hdr.is_large_object()
                    && hdr.block_size as usize == block_size
                    && hdr.needs_sweep()
                    && hdr.dead_count() > 0
                    && !hdr.all_dead()
            };
            if !matches {
                self.pending_sweep_by_class[class_index].swap_remove(i);
                continue;
            }
            let reclaimed = unsafe { crate::gc::sweep_specific_page(self, page_ptr, 1) };
            if reclaimed > 0 {
                if let Some(ptr) = self.alloc_from_free_list(class_index) {
                    return Some(ptr);
                }
            }
            unsafe {
                let header = page_ptr.as_ptr();
                if header.read().dead_count() == 0 {
                    self.pending_sweep_by_class[class_index].swap_remove(i);
                    continue;
                }
            }
            i += 1;
        }
        None
    }

    /// Try to allocate from the free list of an existing page.
    ///
    /// Uses a per-size-class preferred page cache for O(1) allocation when the
    /// cached page has free slots. Falls back to O(P) scan over `pages_with_free_slots`
    /// (P = pages with space), or O(K) over `pages_by_class` if the free-slots list is empty.
    fn alloc_from_free_list(&mut self, class_index: usize) -> Option<NonNull<u8>> {
        let block_size = SIZE_CLASSES[class_index];

        // Fast path: try preferred page first if cached and valid
        if let Some(page_ptr) = self.free_list_preferred[class_index] {
            if let Some((ptr, exhausted)) =
                unsafe { Self::try_pop_from_page(page_ptr.as_ptr(), block_size, true) }
            {
                if exhausted {
                    self.free_list_preferred[class_index] = None;
                    if let Some(pos) = self.pages_with_free_slots[class_index]
                        .iter()
                        .position(|&p| p == page_ptr)
                    {
                        self.pages_with_free_slots[class_index].swap_remove(pos);
                    }
                }
                return Some(ptr);
            }
            self.free_list_preferred[class_index] = None;
        }

        // O(P) scan over pages that have free slots
        let i = 0;
        while i < self.pages_with_free_slots[class_index].len() {
            let page_ptr = self.pages_with_free_slots[class_index][i];
            if let Some((ptr, exhausted)) =
                unsafe { Self::try_pop_from_page(page_ptr.as_ptr(), block_size, true) }
            {
                if exhausted {
                    self.pages_with_free_slots[class_index].swap_remove(i);
                } else {
                    self.free_list_preferred[class_index] = Some(page_ptr);
                }
                return Some(ptr);
            }
            // Page in list but try_pop failed (corrupt/stale) - remove and continue
            self.pages_with_free_slots[class_index].swap_remove(i);
        }

        // Fallback: O(K) scan over all pages (preserves correctness for edge cases)
        for page_ptr in &self.pages_by_class[class_index] {
            if let Some((ptr, exhausted)) =
                unsafe { Self::try_pop_from_page(page_ptr.as_ptr(), block_size, false) }
            {
                if !exhausted {
                    self.free_list_preferred[class_index] = Some(*page_ptr);
                    self.pages_with_free_slots[class_index].push(*page_ptr);
                }
                return Some(ptr);
            }
        }
        None
    }

    #[cfg(feature = "lazy-sweep")]
    /// Fill `out` with up to `max` pointers from the free list using batch pop.
    /// Returns `true` if any pointers were added, `false` to fall back to single-pop.
    fn alloc_from_free_list_batch(
        &mut self,
        class_index: usize,
        out: &mut Vec<NonNull<u8>>,
        max: usize,
    ) -> bool {
        let block_size = SIZE_CLASSES[class_index];

        if let Some(page_ptr) = self.free_list_preferred[class_index] {
            if let Some((ptrs, exhausted)) =
                unsafe { Self::try_pop_batch_from_page(page_ptr.as_ptr(), block_size, max, true) }
            {
                if !ptrs.is_empty() {
                    out.extend(ptrs);
                    if exhausted {
                        self.free_list_preferred[class_index] = None;
                        if let Some(pos) = self.pages_with_free_slots[class_index]
                            .iter()
                            .position(|&p| p == page_ptr)
                        {
                            self.pages_with_free_slots[class_index].swap_remove(pos);
                        }
                    } else {
                        self.free_list_preferred[class_index] = Some(page_ptr);
                    }
                    return true;
                }
            }
            self.free_list_preferred[class_index] = None;
        }

        let i = 0;
        while i < self.pages_with_free_slots[class_index].len() {
            let page_ptr = self.pages_with_free_slots[class_index][i];
            if let Some((ptrs, exhausted)) =
                unsafe { Self::try_pop_batch_from_page(page_ptr.as_ptr(), block_size, max, true) }
            {
                if !ptrs.is_empty() {
                    out.extend(ptrs);
                    if exhausted {
                        self.pages_with_free_slots[class_index].swap_remove(i);
                    } else {
                        self.free_list_preferred[class_index] = Some(page_ptr);
                    }
                    return true;
                }
            }
            self.pages_with_free_slots[class_index].swap_remove(i);
        }

        // Fallback: O(K) scan over pages_by_class (mirrors single-pop fallback)
        for page_ptr in &self.pages_by_class[class_index] {
            if let Some((ptrs, exhausted)) =
                unsafe { Self::try_pop_batch_from_page(page_ptr.as_ptr(), block_size, max, false) }
            {
                if !ptrs.is_empty() {
                    out.extend(ptrs);
                    if !exhausted {
                        self.free_list_preferred[class_index] = Some(*page_ptr);
                        self.pages_with_free_slots[class_index].push(*page_ptr);
                    }
                    return true;
                }
            }
        }

        false
    }

    /// Try to pop one object from a page's free list.
    ///
    /// Returns `Some((ptr, exhausted))` on success, where `exhausted` is true
    /// if the page has no more free slots after this pop. Returns `None` if
    /// the page has no free slots or slot is stale (`allocated_bitmap` out of sync).
    ///
    /// When `known_small` is true, skips the `is_large_object` check. Only use when
    /// the page is from `free_list_preferred` or `pages_with_free_slots`.
    #[allow(clippy::cognitive_complexity)]
    /// # Safety
    /// `header` must point to a valid `PageHeader` for a page owned by this heap.
    unsafe fn try_pop_from_page(
        header: *mut PageHeader,
        block_size: usize,
        known_small: bool,
    ) -> Option<(NonNull<u8>, bool)> {
        // SAFETY: Caller guarantees header is valid.
        if !known_small && unsafe { (*header).is_large_object() } {
            return None;
        }
        if unsafe { (*header).block_size as usize } != block_size {
            return None;
        }
        // SAFETY: Caller guarantees header is valid.
        let idx = unsafe { (*header).free_list_head_raw() };
        if idx == PageHeader::FREE_LIST_NONE {
            return None;
        }
        // Sanity check: ensure slot is not already allocated
        // This can happen if free list and allocated_bitmap are out of sync
        // due to concurrent sweep/allocate operations.
        // SAFETY: Caller guarantees header is valid.
        if unsafe { (*header).is_allocated(idx as usize) } {
            // Slot is allocated but in free list - corrupt. Pop it and give up on this page.
            // Do NOT read next_head from slot memory (it contains user data, not a list ptr).
            // Clear the free list head to avoid leaving corrupt state; sweep will rebuild.
            // SAFETY: Caller guarantees header is valid.
            let _ = unsafe { (*header).compare_exchange_free_list(Some(idx), None) };
            return None;
        }
        // SAFETY: Caller guarantees header is valid.
        let h_size = unsafe { (*header).header_size as usize };
        let page_addr = header.cast::<u8>();
        // SAFETY: Header is valid, offsets are within page bounds.
        let obj_ptr = unsafe { page_addr.add(h_size + (idx as usize * block_size)) };

        // Popping from free list: read the next pointer stored in the slot.
        // SAFETY: sweep_page (copy_sweep_logic) ensures this is a valid Option<u16>.
        let next_head = unsafe { obj_ptr.cast::<Option<u16>>().read_unaligned() };
        let exhausted = next_head.is_none();

        // Pop from free list atomically using CAS
        loop {
            // SAFETY: Caller guarantees header is valid.
            if unsafe { (*header).compare_exchange_free_list(Some(idx), next_head) }.is_ok() {
                break;
            }
        }

        // Mark as allocated so it's tracked during sweep
        // SAFETY: Caller guarantees header is valid.
        unsafe { (*header).set_allocated(idx as usize) };

        // Clear ALL_DEAD flag since we're allocating a new live object
        // SAFETY: Caller guarantees header is valid.
        if unsafe { (*header).all_dead() } {
            unsafe {
                (*header).clear_all_dead();
                (*header).set_dead_count(0);
            }
        }

        Some((unsafe { NonNull::new_unchecked(obj_ptr) }, exhausted))
    }

    #[cfg(feature = "lazy-sweep")]
    /// Try to pop up to `batch_size` objects from a page's free list.
    ///
    /// Returns `Some((ptrs, exhausted))` where `exhausted` means no slots remain.
    /// Uses one CAS for the entire batch instead of N.
    ///
    /// When `known_small` is true, skips the `is_large_object` check. Only use when
    /// the page is from `free_list_preferred` or `pages_with_free_slots`.
    ///
    /// # Safety
    /// `header` must point to a valid `PageHeader` for a page owned by this heap.
    #[allow(clippy::cognitive_complexity)]
    unsafe fn try_pop_batch_from_page(
        header: *mut PageHeader,
        block_size: usize,
        batch_size: usize,
        known_small: bool,
    ) -> Option<(Vec<NonNull<u8>>, bool)> {
        // SAFETY: Caller guarantees header is valid.
        if !known_small && unsafe { (*header).is_large_object() } {
            return None;
        }
        if unsafe { (*header).block_size as usize } != block_size {
            return None;
        }
        // SAFETY: Caller guarantees header is valid.
        let first_idx = unsafe { (*header).free_list_head_raw() };
        if first_idx == PageHeader::FREE_LIST_NONE {
            return None;
        }
        // SAFETY: Caller guarantees header is valid.
        if unsafe { (*header).is_allocated(first_idx as usize) } {
            let _ = unsafe { (*header).compare_exchange_free_list(Some(first_idx), None) };
            return None;
        }

        let h_size = unsafe { (*header).header_size as usize };
        let page_addr = header.cast::<u8>();

        let mut ptrs = Vec::with_capacity(batch_size);
        let mut idx = first_idx;
        let mut next_head: Option<u16> = None;

        for _ in 0..batch_size {
            // SAFETY: Caller guarantees header is valid.
            if unsafe { (*header).is_allocated(idx as usize) } {
                next_head = None;
                break;
            }
            let obj_ptr = unsafe { page_addr.add(h_size + (idx as usize * block_size)) };
            let next = unsafe { obj_ptr.cast::<Option<u16>>().read_unaligned() };
            ptrs.push(unsafe { NonNull::new_unchecked(obj_ptr) });
            next_head = next;
            match next {
                Some(n) => idx = n,
                None => break,
            }
        }

        let exhausted = next_head.is_none();

        loop {
            // SAFETY: Caller guarantees header is valid.
            if unsafe { (*header).compare_exchange_free_list(Some(first_idx), next_head) }.is_ok() {
                break;
            }
        }

        for ptr in &ptrs {
            let offset = ptr
                .as_ptr()
                .addr()
                .saturating_sub(page_addr.addr())
                .saturating_sub(h_size);
            let slot_idx = offset / block_size;
            // SAFETY: Caller guarantees header is valid.
            unsafe { (*header).set_allocated(slot_idx) };
        }

        if !ptrs.is_empty() && unsafe { (*header).all_dead() } {
            unsafe {
                (*header).clear_all_dead();
                (*header).set_dead_count(0);
            }
        }

        Some((ptrs, exhausted))
    }

    #[inline(never)]
    /// Try to adopt an orphan page with matching size class.
    ///
    /// Searches for an orphan page that:
    /// - Is not a large object page
    /// - Has the requested `block_size`
    /// - Has sufficient free slots (at least 25% of capacity)
    ///
    /// Returns `Some(page_addr)` if a suitable page is found and removed from `orphan_by_addr`.
    /// Returns `None` if no suitable page is available.
    ///
    /// # Note
    ///
    /// This method is reserved for future orphan page adoption mechanism.
    /// Currently, it always returns `None`.
    pub const fn try_adopt_orphan_page(&mut self, _block_size: usize) -> Option<usize> {
        None
    }
    /// Allocation slow path - requests new page from global manager.
    ///
    /// # Reentrant Safety
    ///
    /// **NOT reentrant-safe during GC.** This function modifies:
    /// - `self.pages` vector (push)
    /// - `self.small_pages` set (insert)
    ///
    /// Never call this from GC phases that iterate over pages.
    /// Use the snapshot pattern in `sweep_phase1_finalize` instead.
    ///
    /// See `docs/reentrant-alloc-rules.md` for safety guidelines.
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
            .unwrap_or_else(PoisonError::into_inner)
            .allocate_page(crate::heap::page_size(), boundary);

        // 2. Initialize Page Header
        // SAFETY: ptr is page-aligned
        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<PageHeader>();
        let obj_count = PageHeader::max_objects(block_size);
        let h_size = PageHeader::header_size(block_size);

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
                flags: AtomicU8::new(0),
                owner_thread: get_thread_id(),
                #[cfg(feature = "lazy-sweep")]
                dead_count: AtomicU16::new(0),
                #[cfg(not(feature = "lazy-sweep"))]
                _padding: [0; 2],
                mark_bitmap: core::array::from_fn(|_| AtomicU64::new(0)),
                dirty_bitmap: core::array::from_fn(|_| AtomicU64::new(0)),
                allocated_bitmap: core::array::from_fn(|_| AtomicU64::new(0)),
                #[cfg(feature = "lazy-sweep")]
                free_list_head: AtomicU16::new(PageHeader::FREE_LIST_NONE),
                #[cfg(not(feature = "lazy-sweep"))]
                free_list_head: 0,
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
        // SAFETY: Snapshot pattern in callers makes this safe during GC.
        // See docs/reentrant-alloc-rules.md.
        self.pages.push(header);
        self.small_pages.insert(ptr.as_ptr() as usize);
        self.pages_by_class[class_index].push(header);

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
    /// Panics if the alignment requirement exceeds the page size.
    fn alloc_large(&mut self, size: usize, align: usize) -> NonNull<u8> {
        check_safepoint();

        assert!(
            page_size() >= align,
            "Type alignment ({align}) exceeds page size ({}). \
              Such extreme alignment requirements are not supported.",
            page_size()
        );

        // For large objects, allocate dedicated pages
        // The header must be followed by padding to satisfy the object's alignment.
        let base_h_size = PageHeader::header_size(size);
        let h_size = (base_h_size + align - 1) & !(align - 1);
        let total_size = h_size + size;
        let pages_needed = total_size.div_ceil(page_size());
        let alloc_size = pages_needed * page_size();

        // Use safe allocation logic
        // Create boundary to filter out our own stack frame
        let marker = 0;
        let boundary = std::ptr::addr_of!(marker) as usize;
        let (ptr, _) = segment_manager()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .allocate_page(alloc_size, boundary);

        // ptr is NonNull<u8> already check for null logic inside allocate_safe_page

        // SAFETY: ptr is page-aligned, which is more strict than PageHeader's alignment.
        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<PageHeader>();
        // SAFETY: We just allocated this memory
        unsafe {
            header.as_ptr().write(PageHeader {
                magic: MAGIC_GC_PAGE,
                #[allow(clippy::cast_possible_truncation)]
                block_size: size as u32,
                obj_count: 1,
                #[allow(clippy::cast_possible_truncation)]
                header_size: h_size as u16,
                generation: 0,
                flags: AtomicU8::new(PAGE_FLAG_LARGE),
                owner_thread: get_thread_id(),
                #[cfg(feature = "lazy-sweep")]
                dead_count: AtomicU16::new(0),
                #[cfg(not(feature = "lazy-sweep"))]
                _padding: [0; 2],
                mark_bitmap: core::array::from_fn(|_| AtomicU64::new(0)),
                dirty_bitmap: core::array::from_fn(|_| AtomicU64::new(0)),
                allocated_bitmap: core::array::from_fn(|_| AtomicU64::new(0)),
                #[cfg(feature = "lazy-sweep")]
                free_list_head: AtomicU16::new(PageHeader::FREE_LIST_NONE),
                #[cfg(not(feature = "lazy-sweep"))]
                free_list_head: 0,
            });
            // Mark the single object as allocated
            (*header.as_ptr()).set_allocated(0);

            // Initialize the GcBox with no-op drop/trace to prevent crashes if
            // the caller doesn't properly initialize the GcBox (e.g., when using
            // low-level alloc API directly in tests).
            #[allow(clippy::cast_ptr_alignment)]
            let gc_box_ptr = ptr.as_ptr().add(h_size).cast::<crate::ptr::GcBox<()>>();
            std::ptr::addr_of_mut!((*gc_box_ptr).drop_fn)
                .write(crate::ptr::GcBox::<()>::no_op_drop);
            std::ptr::addr_of_mut!((*gc_box_ptr).trace_fn)
                .write(crate::ptr::GcBox::<()>::no_op_trace);
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
            let page_addr = header_addr + (p * page_size());
            self.large_object_map
                .insert(page_addr, (header_addr, size, h_size));
            // Register in global manager too?
            segment_manager()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
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
            .filter(|p| unsafe { p.as_ptr().read().is_large_object() })
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
            .filter(|p| unsafe { p.as_ptr().read().is_large_object() })
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

    /// Deallocate memory allocated by `alloc`.
    ///
    /// This is used for panic cleanup in `new_cyclic_weak` when the construction
    /// closure panics before the value is written to the `GcBox`.
    ///
    /// # Safety
    ///
    /// - `ptr` must have been returned by a previous call to `alloc` on `self`
    /// - `size` and `align` must match what was passed to `alloc`
    /// - The memory at `ptr` must not be accessed after deallocation
    ///
    /// # Panics
    ///
    /// Panics if the segment manager lock is poisoned.
    #[allow(unsafe_op_in_unsafe_fn)]
    pub unsafe fn dealloc(&mut self, ptr: NonNull<u8>) {
        let addr = ptr.as_ptr() as usize;

        let page_addr = addr & page_mask();

        if let Some(&(header_addr, size, header_size)) = self.large_object_map.get(&page_addr) {
            let total_size = header_size + size;
            let alloc_size = total_size.div_ceil(page_size()) * page_size();

            let gc_box_ptr = addr as *mut crate::ptr::GcBox<()>;
            if !(*gc_box_ptr).has_dead_flag() {
                ((*gc_box_ptr).drop_fn)(addr as *mut u8);
            }

            for p in 0..(alloc_size / page_size()) {
                let page = header_addr + (p * page_size());
                self.large_object_map.remove(&page);
                segment_manager()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .large_object_map
                    .remove(&page);
            }

            // Deallocate the memory
            unsafe {
                sys_alloc::Mmap::from_raw(addr as *mut u8, alloc_size);
            }
        } else if self.small_pages.contains(&page_addr) {
            // It's a small object - find the page header
            for page_ptr in &self.pages {
                if page_ptr.as_ptr() as usize == page_addr {
                    let header = page_ptr.as_ptr();
                    let is_large = unsafe { (*header).is_large_object() };
                    if !is_large {
                        let block_size = unsafe { (*header).block_size as usize };
                        let header_size = PageHeader::header_size(block_size);
                        let obj_count = unsafe { (*header).obj_count as usize };
                        let idx = (addr - page_addr - header_size) / block_size;

                        if idx < obj_count {
                            // Drop the value if it was initialized
                            let obj_ptr = addr as *mut u8;
                            #[allow(clippy::cast_ptr_alignment)]
                            let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                            if !unsafe { (*gc_box_ptr).has_dead_flag() } {
                                unsafe { ((*gc_box_ptr).drop_fn)(obj_ptr) };
                            }

                            // Add back to free list
                            unsafe {
                                let mut next_head = (*header).free_list_head();
                                obj_ptr.cast::<Option<u16>>().write_unaligned(next_head);
                                // Push to free list atomically using CAS
                                loop {
                                    let old = next_head.unwrap_or(u16::MAX);
                                    match (*header).free_list_head.compare_exchange(
                                        old,
                                        u16::try_from(idx).unwrap(),
                                        Ordering::AcqRel,
                                        Ordering::Acquire,
                                    ) {
                                        Ok(_) => break,
                                        Err(actual) => {
                                            next_head = if actual == u16::MAX {
                                                None
                                            } else {
                                                Some(actual)
                                            };
                                            obj_ptr
                                                .cast::<Option<u16>>()
                                                .write_unaligned(next_head);
                                        }
                                    }
                                }
                                (*header).clear_allocated(idx);
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
}

/// Unified write barrier for generational GC.
///
/// This function handles both small and large objects, sets the per-object
/// dirty bit, and adds the page to the dirty pages list.
///
/// # Arguments
/// * `ptr` - Raw pointer to the field being mutated (not the containing object)
#[allow(dead_code)]
#[inline]
pub fn simple_write_barrier(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }

    let ptr_addr = ptr as usize;
    let heap_start = heap_start();
    let heap_end = heap_end();

    if ptr_addr < heap_start || ptr_addr > heap_end {
        return;
    }

    unsafe {
        let header = ptr_to_page_header(ptr);

        if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
            return;
        }

        if (*header.as_ptr()).generation == 0 {
            return;
        }

        let block_size = (*header.as_ptr()).block_size as usize;
        let header_size = (*header.as_ptr()).header_size as usize;
        let header_page_addr = header.as_ptr() as usize;

        if ptr_addr < header_page_addr + header_size {
            return;
        }

        let offset = ptr_addr - (header_page_addr + header_size);
        let index = offset / block_size;
        let obj_count = (*header.as_ptr()).obj_count as usize;

        if index >= obj_count {
            return;
        }

        (*header.as_ptr()).set_dirty(index);

        crate::heap::with_heap(|heap| {
            heap.add_to_dirty_pages(header);
        });
    }
}

/// Unified incremental write barrier (SATB remembered set).
///
/// Records old-generation pages in the remembered buffer for incremental GC.
#[allow(dead_code)]
#[inline]
pub fn incremental_write_barrier(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }

    let ptr_addr = ptr as usize;
    let heap_start = heap_start();
    let heap_end = heap_end();

    if ptr_addr < heap_start || ptr_addr > heap_end {
        return;
    }

    // SAFETY: This fence synchronizes with the GC thread to ensure
    // that all prior writes are visible before we record in the remembered set.
    // Required for SATB correctness in incremental GC.
    std::sync::atomic::fence(Ordering::AcqRel);

    unsafe {
        let header = ptr_to_page_header(ptr);

        if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
            return;
        }

        if (*header.as_ptr()).generation == 0 {
            return;
        }

        crate::heap::with_heap(|heap| {
            heap.record_in_remembered_buffer(header);
        });
    }
}

impl Default for LocalHeap {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LocalHeap {
    fn drop(&mut self) {
        let current_thread = std::thread::current().id();

        let mut manager = segment_manager()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        for page_ptr in std::mem::take(&mut self.pages) {
            unsafe {
                let header = page_ptr.as_ptr();

                if (*header).magic != MAGIC_GC_PAGE {
                    continue;
                }

                let is_large = (*header).is_large_object();
                let block_size = (*header).block_size as usize;
                let header_size = (*header).header_size as usize;

                let size = if is_large {
                    let total = header_size + block_size;
                    total.div_ceil(page_size()) * page_size()
                } else {
                    page_size()
                };

                (*header).set_orphan();

                let addr = page_ptr.as_ptr() as usize;
                manager.orphan_by_addr.insert(
                    addr,
                    OrphanPage {
                        addr,
                        size,
                        is_large,
                        original_owner: current_thread,
                    },
                );
            }
        }
        drop(manager);

        self.large_object_map.clear();
        self.small_pages.clear();
    }
}

/// Sweep and reclaim orphan pages.
///
/// # Panics
///
/// Panics if the segment manager lock is poisoned.
pub fn sweep_orphan_pages() {
    let mut manager = segment_manager()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let mut to_reclaim = Vec::new();

    manager.orphan_by_addr.retain(|_addr, orphan| unsafe {
        let header = orphan.addr as *mut PageHeader;
        let is_large = (*header).is_large_object();

        let has_survivors = if is_large {
            (*header).is_marked(0)
        } else {
            let obj_count = (*header).obj_count as usize;
            (0..obj_count).any(|i| (*header).is_marked(i))
        };

        let has_weak_refs = if is_large {
            let header_size = (*header).header_size as usize;
            let obj_ptr = (orphan.addr as *mut u8).add(header_size);
            #[allow(clippy::cast_ptr_alignment)]
            let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
            (*gc_box_ptr).weak_count() > 0
        } else {
            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);

            (0..obj_count).any(|i| {
                if (*header).is_allocated(i) {
                    let obj_ptr = (orphan.addr as *mut u8).add(header_size + i * block_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                    (*gc_box_ptr).weak_count() > 0
                } else {
                    false
                }
            })
        };

        if has_survivors || has_weak_refs {
            (*header).clear_all_marks();
            true
        } else {
            to_reclaim.push((orphan.addr, orphan.size, is_large, header as usize));
            false
        }
    });

    drop(manager);

    // Phase 1: Finalize (call drop_fn) for all doomed objects.
    // We do this BEFORE unmapping any memory because objects may have
    // cross-page references.
    for &(addr, _size, _is_large, _header_addr) in &to_reclaim {
        unsafe {
            let header = addr as *mut PageHeader;
            let is_large = (*header).is_large_object();

            if is_large {
                let header_size = (*header).header_size as usize;
                let obj_ptr = (addr as *mut u8).add(header_size);
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                if !(*gc_box_ptr).has_dead_flag() {
                    ((*gc_box_ptr).drop_fn)(obj_ptr);
                }
            } else {
                let block_size = (*header).block_size as usize;
                let obj_count = (*header).obj_count as usize;
                let header_size = PageHeader::header_size(block_size);

                for i in 0..obj_count {
                    if (*header).is_allocated(i) {
                        let obj_ptr = (addr as *mut u8).add(header_size + i * block_size);
                        #[allow(clippy::cast_ptr_alignment)]
                        let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                        if !(*gc_box_ptr).has_dead_flag() {
                            ((*gc_box_ptr).drop_fn)(obj_ptr);
                        }
                    }
                }
            }
        }
    }

    // Phase 2: Reclaim memory and clean up large_object_map entries.
    for (addr, size, is_large, header_addr) in to_reclaim {
        unsafe {
            sys_alloc::Mmap::from_raw(addr as *mut u8, size);
        }

        if is_large {
            let mut manager = segment_manager()
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let ps = page_size();
            for p in 0..(size / ps) {
                let page_addr = header_addr + (p * ps);
                manager.large_object_map.remove(&page_addr);
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
        let tcb = std::sync::Arc::new(ThreadControlBlock::new());
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

/// Execute a function with mutable access to the thread-local heap.
#[inline]
pub fn with_heap<F, R>(f: F) -> R
where
    F: FnOnce(&mut LocalHeap) -> R,
{
    HEAP.with(|local| unsafe { f(&mut *local.tcb.heap.get()) })
}

/// Execute a function with mutable access to the thread-local heap.
/// Returns None if called from a thread without an initialized GC heap.
#[inline]
pub fn try_with_heap<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut LocalHeap) -> R,
{
    HEAP.try_with(|local| unsafe { f(&mut *local.tcb.heap.get()) })
        .ok()
}

/// Execute a function with mutable access to the thread-local heap and its control block.
#[allow(dead_code)]
#[inline]
pub fn with_heap_and_tcb<F, R>(f: F) -> R
where
    F: FnOnce(&mut LocalHeap, &ThreadControlBlock) -> R,
{
    HEAP.with(|local| unsafe { f(&mut *local.tcb.heap.get(), &local.tcb) })
}

/// Execute a function with mutable access to the thread-local heap and Arc<ThreadControlBlock>.
#[allow(dead_code)]
#[inline]
pub fn with_heap_and_tcb_arc<F, R>(f: F) -> R
where
    F: FnOnce(&mut LocalHeap, &std::sync::Arc<ThreadControlBlock>) -> R,
{
    HEAP.with(|local| {
        let tcb_arc = local.tcb.clone();
        unsafe { f(&mut *local.tcb.heap.get(), &tcb_arc) }
    })
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

/// Mark the page containing a pointer as dirty.
///
/// This is used by container `Trace` implementations (like `Vec`) to ensure
/// that when a container holding `Gc` pointers is traced, the container's
/// storage buffer's page is marked dirty so GC will scan it.
///
/// # Safety
///
/// The pointer must be valid and readable. The pointer does not need to be
/// aligned or point to the start of an object.
#[inline]
pub unsafe fn mark_page_dirty_for_ptr(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }

    let page_addr = ptr as usize & page_mask();

    HEAP.with(|local| {
        let heap = unsafe { &mut *local.tcb.heap.get() };

        if heap.small_pages.contains(&page_addr) {
            let header = unsafe { ptr_to_page_header(ptr) };
            unsafe { heap.add_to_dirty_pages(header) };
        }
    });
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
    let page_addr = (ptr as usize) & page_mask();
    // SAFETY: Caller guarantees ptr is within a valid GC page.
    unsafe { NonNull::new_unchecked(page_addr as *mut PageHeader) }
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
    // 1. Quick range check — if not in this heap, try orphan/global fallback
    if !heap.is_in_range(addr) {
        return unsafe { find_gc_box_from_orphan(ptr) };
    }

    // 1.1. Safety check for the zero page (first 4KB)
    if addr < 4096 {
        return None;
    }

    // 2. Interior pointer support: allow pointers to any field, not just usize-aligned.
    //    A u32 field may be at offset 4, which is valid for u32 but not for usize (8-byte).
    //    For conservative GC, we need to accept any potentially valid pointer alignment.
    //    Minimum alignment is 1 byte (no alignment requirement for interior pointers).
    unsafe {
        // Note: We removed the usize alignment check here to support interior pointers
        // to fields smaller than usize (e.g., u32, u16, u8). The page header and offset
        // calculations will validate whether this is a valid object pointer.

        // 3. Check large object map first (handles multi-page objects and avoids reading uninit tail pages)
        let page_addr = addr & crate::heap::page_mask();
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
                        .find(|p| p.as_ptr() as usize == (addr & crate::heap::page_mask()))
                        .map_or(header_ptr, |p| p.as_ptr());
                }

                if !heap
                    .small_pages
                    .contains(&(addr & crate::heap::page_mask()))
                {
                    return find_gc_box_from_orphan(ptr);
                }
                if (header_ptr as usize) % 4096 != 0 || (header_ptr as usize) < 4096 {
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

        // 5.1. Allocation check for small objects (Large objects are always allocated if in map)
        if !header.is_large_object() && !header.is_allocated(index) {
            return None;
        }

        // 6. Large object handling: with the map, we now support interior pointers!
        // For large objects, we ensure the pointer is within the allocated bounds.
        if header.is_large_object() {
            if offset_to_use >= block_size_to_use {
                return None;
            }
        } else {
            // Small object interior pointer support.
            // offset_to_use is already relative to the start of the value area.
            // Floor division automatically handles interior pointers.
            // No additional adjustment needed beyond the initial index calculation.
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

/// Try to resolve a pointer from orphan pages or the global large object map.
///
/// This is the fallback path for `find_gc_box_from_ptr` when no live
/// `LocalHeap` contains the target address. It handles:
/// - Large objects: via `segment_manager().large_object_map`
/// - Small objects: via `segment_manager().orphan_by_addr`
///
/// # Safety
///
/// The pointer must be safe to read if it is a valid pointer. Orphan page
/// memory is valid because `sweep_orphan_pages` only reclaims after the mark
/// phase confirms no survivors; this function is used during marking.
#[must_use]
pub unsafe fn find_gc_box_from_orphan(ptr: *const u8) -> Option<NonNull<crate::ptr::GcBox<()>>> {
    let addr = ptr as usize;
    if addr < 4096 {
        return None;
    }

    let page_addr = addr & page_mask();

    let manager = segment_manager()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    // 1. Try global large_object_map (covers multi-page large objects from any thread)
    if let Some(&(head_addr, size, h_size)) = manager.large_object_map.get(&page_addr) {
        drop(manager); // release lock before pointer arithmetic

        if addr < head_addr + h_size {
            return None; // points into header area
        }
        let offset = addr - (head_addr + h_size);
        if offset >= size {
            return None; // past the object
        }
        let obj_ptr = (head_addr as *mut u8).wrapping_add(h_size);
        #[allow(clippy::cast_ptr_alignment)]
        return Some(unsafe { NonNull::new_unchecked(obj_ptr.cast::<crate::ptr::GcBox<()>>()) });
    }

    // 2. Try orphan small pages (O(1) lookup)
    if let Some(orphan) = manager.orphan_by_addr.get(&page_addr) {
        if !orphan.is_large {
            let orphan_addr = orphan.addr;
            let header = orphan_addr as *mut PageHeader;
            let maybe_slot = unsafe {
                if (*header).magic == MAGIC_GC_PAGE {
                    let b_size = (*header).block_size as usize;
                    let h = PageHeader::header_size(b_size);
                    if addr < orphan_addr + h {
                        None // points into header
                    } else {
                        let offset = addr - (orphan_addr + h);
                        let idx = offset / b_size;
                        if idx >= (*header).obj_count as usize || !(*header).is_allocated(idx) {
                            None
                        } else {
                            Some((b_size, h, idx))
                        }
                    }
                } else {
                    None
                }
            };
            if let Some((block_size, h_size, index)) = maybe_slot {
                drop(manager);
                let obj_ptr = (orphan_addr as *mut u8).wrapping_add(h_size + index * block_size);
                #[allow(clippy::cast_ptr_alignment)]
                return Some(unsafe {
                    NonNull::new_unchecked(obj_ptr.cast::<crate::ptr::GcBox<()>>())
                });
            }
        }
    }

    None
}

// ============================================================================
// Test Isolation Support
// ============================================================================

/// Clear all pages in the current thread's local heap.
/// This is used internally by `reset_for_testing`.
#[allow(dead_code)]
fn clear_local_heap() {
    HEAP.with(|local| unsafe {
        let heap = &mut *local.tcb.heap.get();

        // Reset all page headers before clearing the pages vector
        for page_ptr in &heap.pages {
            let header = page_ptr.as_ptr();
            // Reset free list head
            (*header).set_free_list_head(None);
            // Clear allocated bitmap
            for word in &(*header).allocated_bitmap {
                word.store(0u64, Ordering::Release);
            }
            // Clear mark bitmap
            for word in &mut (*header).mark_bitmap {
                word.store(0u64, Ordering::Release);
            }
            // Clear dirty bitmap
            for word in &mut (*header).dirty_bitmap {
                word.store(0u64, Ordering::Release);
            }
        }

        heap.pages.clear();
        heap.small_pages.clear();
        heap.large_object_map.clear();
        for vec in &mut heap.pages_by_class {
            vec.clear();
        }
        for vec in &mut heap.pages_with_free_slots {
            vec.clear();
        }
        for vec in &mut heap.free_list_buffer {
            vec.clear();
        }
        #[cfg(feature = "lazy-sweep")]
        for vec in &mut heap.batch_scratch {
            vec.clear();
        }
        #[cfg(feature = "lazy-sweep")]
        for vec in &mut heap.pending_sweep_by_class {
            vec.clear();
        }
        heap.young_allocated = 0;
        heap.old_allocated = 0;
        heap.min_addr = usize::MAX;
        heap.max_addr = 0;

        // Clear dirty page tracking to prevent dangling pointers and stale state
        heap.dirty_pages.lock().clear();
        heap.dirty_pages_snapshot.clear();
    });
}

/// Reset all global GC state for testing purposes.
///
/// This function clears:
/// - Thread registry (unregisters all threads)
/// - Segment manager (frees all pages)
/// - GC requested flag
/// - Current thread's local heap
///
/// # Safety
///
/// This function is intended for use in tests only. It must not be called
/// while other threads are actively using the GC heap, as this will cause
/// undefined behavior.
///
/// # Example
///
/// ```ignore
/// use rudo_gc::test_util::reset;
///
/// #[test]
/// fn my_test() {
///     reset();
///     // Test code with clean GC state
/// }
/// ```
#[allow(dead_code)]
pub unsafe fn reset_for_testing() {
    // Clear GC requested flag
    GC_REQUESTED.store(false, Ordering::SeqCst);

    // Clear thread registry (handle PoisonError gracefully)
    if let Some(registry) = THREAD_REGISTRY.get() {
        let result = registry.lock();
        if let Ok(mut guard) = result {
            guard.threads.clear();
            guard.active_count.store(0, Ordering::SeqCst);
            guard.gc_in_progress.store(false, Ordering::SeqCst);
        }
    }

    // Clear segment manager (handle PoisonError gracefully)
    if let Some(manager) = SEGMENT_MANAGER.get() {
        let result = manager.lock();
        if let Ok(mut guard) = result {
            guard.free_pages.clear();
            guard.quarantined.clear();
            guard.large_object_map.clear();
            guard.orphan_by_addr.clear();
        }
    }

    // Clear current thread's local heap
    clear_local_heap();
}
