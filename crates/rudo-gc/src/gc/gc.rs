//! Mark-Sweep garbage collection algorithm.
//!
//! This module implements the core garbage collection logic using
//! a mark-sweep algorithm with the `BiBOP` memory layout.

use std::cell::Cell;
use std::collections::HashSet;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::PoisonError;

use crate::gc::incremental::FallbackReason;
use crate::gc::incremental::{
    count_dirty_pages, execute_final_mark, execute_snapshot, mark_slice, IncrementalMarkState,
    MarkPhase, MarkSliceResult, MarkStats,
};
use crate::gc::marker::{
    worker_mark_loop, worker_mark_loop_with_registry, GcWorkerRegistry, ParallelMarkConfig,
    PerThreadMarkQueue,
};
use crate::heap::{LocalHeap, PageHeader};
use crate::ptr::GcBox;
use crate::trace::{GcVisitor, Trace, Visitor, VisitorKind};

#[cfg(feature = "tracing")]
use crate::tracing::internal::{
    log_phase_end, log_phase_end_mark, log_phase_start, next_gc_id, trace_gc_collection,
    trace_phase, GcId, GcPhase,
};

/// Information about an object pending deallocation.
/// Used in two-phase sweep: phase 1 drops, phase 2 reclaims.
///
/// Note: This struct is deprecated as of P1-001 optimization.
/// The sweep phase now uses bitmap checks instead of `PendingDrop` tracking.
#[allow(dead_code)]
struct PendingDrop {
    page: NonNull<PageHeader>,
    index: usize,
}

// ============================================================================
// Collection statistics
// ============================================================================

/// Statistics about the current heap state, used to determine when to collect.
#[derive(Debug, Clone, Copy)]
pub struct CollectInfo {
    /// Number of Gc pointers dropped since last collection.
    n_gcs_dropped: usize,
    /// Number of Gc pointers currently existing.
    n_gcs_existing: usize,
    /// Total bytes allocated in heap.
    heap_size: usize,
    /// Bytes in young generation.
    young_size: usize,
    /// Bytes in old generation.
    old_size: usize,
}

impl CollectInfo {
    /// Number of Gc pointers dropped since last collection.
    #[must_use]
    pub const fn n_gcs_dropped_since_last_collect(&self) -> usize {
        self.n_gcs_dropped
    }

    /// Number of Gc pointers currently existing.
    #[must_use]
    pub const fn n_gcs_existing(&self) -> usize {
        self.n_gcs_existing
    }

    /// Total bytes allocated in heap.
    #[must_use]
    pub const fn heap_size(&self) -> usize {
        self.heap_size
    }

    /// Bytes in young generation.
    #[must_use]
    pub const fn young_size(&self) -> usize {
        self.young_size
    }

    /// Bytes in old generation.
    #[must_use]
    pub const fn old_size(&self) -> usize {
        self.old_size
    }
}

// ============================================================================
// Collection condition
// ============================================================================

/// Type for collection condition functions.
pub type CollectCondition = fn(&CollectInfo) -> bool;

/// The default collection condition.
///
/// Returns `true` when `n_gcs_dropped > n_gcs_existing`, ensuring
/// amortized O(1) collection overhead.
/// The default collection condition.
///
/// Returns `true` if we should run *some* collection.
/// The detailed decision (Minor vs Major) is made in `collect()`.
#[must_use]
pub const fn default_collect_condition(info: &CollectInfo) -> bool {
    // Simple heuristic: Collect if we dropped more than existing, OR young gen is large
    info.n_gcs_dropped > info.n_gcs_existing || info.young_size > 1024 * 1024 // 1MB young limit
}

// ============================================================================
// Thread-local GC state
// ============================================================================

thread_local! {
    /// Number of Gc pointers dropped since last collection.
    static N_DROPS: Cell<usize> = const { Cell::new(0) };
    /// Number of Gc pointers currently existing.
    static N_EXISTING: Cell<usize> = const { Cell::new(0) };
    /// The current collection condition.
    static COLLECT_CONDITION: Cell<CollectCondition> = const { Cell::new(default_collect_condition) };
    /// Whether a collection is currently in progress.
    static IN_COLLECT: Cell<bool> = const { Cell::new(false) };

    static TEST_ROOTS: std::cell::RefCell<Vec<*const u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Register a root for GC marking. This is useful for tests where Miri cannot find
/// roots via conservative stack scanning.
pub fn register_test_root(ptr: *const u8) {
    TEST_ROOTS.with(|roots| roots.borrow_mut().push(ptr));
}

/// Clear all registered test roots.
pub fn clear_test_roots() {
    TEST_ROOTS.with(|roots| roots.borrow_mut().clear());
}

/// Iterate over registered test roots.
#[cfg(any(test, feature = "test-util"))]
pub fn iter_test_roots<F, R>(f: F) -> R
where
    F: FnOnce(&std::cell::RefCell<Vec<*const u8>>) -> R,
{
    TEST_ROOTS.with(f)
}

/// Notify that a Gc was created.
pub fn notify_created_gc() {
    N_EXISTING.with(|n| n.set(n.get() + 1));
}

/// Notify that a `Gc` was dropped, potentially triggering collection.
pub fn notify_dropped_gc() {
    N_DROPS.with(|n| n.set(n.get() + 1));
    maybe_collect();
}

fn maybe_collect() {
    if IN_COLLECT.with(Cell::get) {
        return;
    }

    let stats = crate::heap::HEAP
        .try_with(|heap| {
            (
                unsafe { &*heap.tcb.heap.get() }.total_allocated(),
                unsafe { &*heap.tcb.heap.get() }.young_allocated(),
                unsafe { &*heap.tcb.heap.get() }.old_allocated(),
            )
        })
        .ok();

    let Some((total, young, old)) = stats else {
        return;
    };

    let info = CollectInfo {
        n_gcs_dropped: N_DROPS.with(Cell::get),
        n_gcs_existing: N_EXISTING.with(Cell::get),
        heap_size: total,
        young_size: young,
        old_size: old,
    };

    let condition = COLLECT_CONDITION.with(Cell::get);
    if condition(&info) {
        collect();
    }
}

/// Returns true if a garbage collection is currently in progress.
#[must_use]
pub fn is_collecting() -> bool {
    IN_COLLECT.with(Cell::get)
}

/// Set the function which determines whether the garbage collector should be run.
pub fn set_collect_condition(f: CollectCondition) {
    COLLECT_CONDITION.with(|c| c.set(f));
}

/// Manually check for a pending GC request and block until it's processed.
///
/// This function should be called in long-running loops that don't perform
/// allocations, to ensure threads can respond to GC requests in a timely manner.
///
/// # Example
///
/// ```
/// use rudo_gc::safepoint;
///
/// for _ in 0..1000 {
///     // Do some non-allocating work...
///     let _: Vec<i32> = (0..100).collect();
///
///     // Check for GC requests
///     safepoint();
/// }
/// ```
pub fn safepoint() {
    crate::heap::check_safepoint();
}

// ============================================================================
// Mark-Sweep Collection
// ============================================================================

const MAJOR_THRESHOLD: usize = 10 * 1024 * 1024; // 10MB

#[inline]
fn log_fallback_reason(reason: FallbackReason) {
    match reason {
        FallbackReason::None => {
            eprintln!("[GC] Incremental marking fallback: unknown reason (atomic not set)");
        }
        FallbackReason::DirtyPagesExceeded => {
            eprintln!("[GC] Incremental marking fallback: dirty pages exceeded threshold");
        }
        FallbackReason::SliceTimeout => {
            eprintln!("[GC] Incremental marking fallback: slice timeout exceeded");
        }
        FallbackReason::WorklistUnbounded => {
            eprintln!("[GC] Incremental marking fallback: worklist grew unbounded");
        }
        FallbackReason::SatbBufferOverflow => {
            eprintln!("[GC] Incremental marking fallback: SATB buffer overflowed");
        }
    }
}

/// Perform a garbage collection.
///
/// Decides between Minor and Major collection based on heuristics.
/// Implements cooperative rendezvous for multi-threaded safety.
pub fn collect() {
    // Reentrancy guard
    if IN_COLLECT.with(Cell::get) {
        return;
    }

    let is_collector = crate::heap::request_gc_handshake();

    if is_collector {
        perform_multi_threaded_collect();
    } else {
        // We're not the collector - atomically clear GC flag and wake threads
        // to prevent race condition where threads enter rendezvous after wake-up
        perform_single_threaded_collect_with_wake();
    }
}

/// Perform collection as the collector thread.
#[allow(
    clippy::too_many_lines,
    clippy::collapsible_else_if,
    clippy::if_not_else
)]
fn perform_multi_threaded_collect() {
    #[cfg(feature = "tracing")]
    let gc_id = next_gc_id();
    #[cfg(feature = "tracing")]
    let _gc_span = trace_gc_collection("major_multi_threaded", gc_id);

    crate::gc::marker::clear_overflow_queue();

    IN_COLLECT.with(|in_collect| in_collect.set(true));

    let start = std::time::Instant::now();
    let before_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    // Reset drop counter
    N_DROPS.with(|n| n.set(0));

    let mut objects_reclaimed = 0;

    // CRITICAL FIX: Set global gc_in_progress flag BEFORE taking thread snapshot
    // This ensures new threads can detect that GC is in progress and avoid
    // participating in rendezvous. The thread-local IN_COLLECT flag can't be
    // used here because newly spawned threads get their own copy (default: false).
    crate::heap::thread_registry()
        .lock()
        .unwrap()
        .set_gc_in_progress(true);

    // Determine collection type based on current thread's heap
    let total_size = crate::heap::HEAP.with(|h| {
        let heap = unsafe { &*h.tcb.heap.get() };
        heap.total_allocated()
    });

    // Collect all stack roots BEFORE processing heaps
    // This ensures we capture roots from all threads before any are consumed
    let tcbs = crate::heap::get_all_thread_control_blocks();
    let all_stack_roots: Vec<(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)> = tcbs
        .iter()
        .flat_map(|tcb| {
            let roots = crate::heap::take_stack_roots(tcb);
            roots.into_iter().map(move |ptr| (ptr, tcb.clone()))
        })
        .collect();

    if total_size > MAJOR_THRESHOLD {
        // CRITICAL FIX: For major GC, we must clear ALL marks first, then mark ALL
        // reachable objects, then sweep ALL heaps. The old approach processed each
        // heap independently, which caused marks on other heaps (set during
        // tracing of cross-heap references) to be cleared when processing those heaps.
        // This led to objects only transitively reachable through other heaps being
        // incorrectly swept, causing use-after-free bugs.

        // Phase 1: Clear all marks on ALL heaps
        #[cfg(feature = "tracing")]
        let _clear_span = trace_phase(GcPhase::Clear);
        #[cfg(feature = "tracing")]
        log_phase_start(GcPhase::Clear, before_bytes);

        for tcb in &tcbs {
            unsafe {
                clear_all_marks_and_dirty(&*tcb.heap.get());
            }
        }

        #[cfg(feature = "tracing")]
        log_phase_end(GcPhase::Clear, 0);

        // Phase 2: Mark all reachable objects (tracing across all heaps)
        #[cfg(feature = "tracing")]
        let _mark_span = trace_phase(GcPhase::Mark);
        #[cfg(feature = "tracing")]
        log_phase_start(GcPhase::Mark, before_bytes);

        // We mark from each heap's perspective to ensure we find all cross-heap references
        super::sync::GC_MARK_IN_PROGRESS.store(true, std::sync::atomic::Ordering::Release);
        let mut total_objects_marked: usize = 0;
        for tcb in &tcbs {
            unsafe {
                total_objects_marked = total_objects_marked.saturating_add(mark_major_roots_multi(
                    &mut *tcb.heap.get(),
                    &all_stack_roots,
                ));
            }
        }
        super::sync::GC_MARK_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Release);

        #[cfg(feature = "tracing")]
        log_phase_end_mark(GcPhase::Mark, total_objects_marked);

        // SAFETY: This fence ensures all mark bitmap writes from the marking phase
        // are visible before any sweeping thread clears marks. Without this fence,
        // a thread could start sweeping and clear marks that haven't yet propagated
        // from a slow marking thread, causing live objects to be swept.
        std::sync::atomic::fence(std::sync::atomic::Ordering::AcqRel);

        // Phase 3: Sweep ALL heaps
        #[cfg(feature = "tracing")]
        let _sweep_span = trace_phase(GcPhase::Sweep);
        #[cfg(feature = "tracing")]
        log_phase_start(GcPhase::Sweep, before_bytes);

        for tcb in &tcbs {
            unsafe {
                #[cfg(feature = "lazy-sweep")]
                {
                    let heap = &*tcb.heap.get();
                    for page_ptr in heap.all_pages() {
                        let header = page_ptr.as_ptr();
                        if !header.read().is_large_object() {
                            let block_size = (*header).block_size as usize;
                            let obj_count = (*header).obj_count as usize;

                            let mut dead_count = 0u16;
                            let mut allocated_count = 0u16;

                            for i in 0..obj_count {
                                if (*header).is_allocated(i) {
                                    allocated_count += 1;
                                    if !(*header).is_marked(i) {
                                        dead_count += 1;
                                    } else {
                                        (*header).clear_mark(i);
                                    }
                                }
                            }

                            if allocated_count > 0 {
                                let total_dead = (*header).dead_count() + dead_count;
                                if total_dead == allocated_count {
                                    (*header).set_all_dead();
                                }
                                (*header).set_needs_sweep();
                                (*header).set_dead_count(total_dead);
                                (*header).clear_all_marks();
                            }
                        } else {
                            (*header).clear_needs_sweep();
                            (*header).clear_all_dead();
                            (*header).set_dead_count(0);
                            if !(*header).is_fully_marked() {
                                let reclaimed = sweep_large_objects(&mut *tcb.heap.get(), false);
                                objects_reclaimed += reclaimed;
                            } else {
                                (*header).clear_all_marks();
                            }
                        }
                    }
                    promote_all_pages(&*tcb.heap.get());
                }
                #[cfg(not(feature = "lazy-sweep"))]
                {
                    let reclaimed = sweep_segment_pages(&*tcb.heap.get(), false);
                    let reclaimed_large = sweep_large_objects(&mut *tcb.heap.get(), false);
                    objects_reclaimed += reclaimed + reclaimed_large;
                    promote_all_pages(&*tcb.heap.get());
                }
            }
        }

        crate::heap::sweep_orphan_pages();

        #[cfg(feature = "tracing")]
        log_phase_end(
            GcPhase::Sweep,
            before_bytes.saturating_sub(
                crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated()),
            ),
        );
    } else {
        // Minor GC doesn't have cross-heap issues since it only scans young objects
        // and uses remembered sets for inter-generational references
        for tcb in &tcbs {
            unsafe {
                objects_reclaimed += collect_minor_multi(&mut *tcb.heap.get(), &all_stack_roots);
            }
        }
    }

    let collection_type = if total_size > MAJOR_THRESHOLD {
        crate::metrics::CollectionType::Major
    } else {
        crate::metrics::CollectionType::Minor
    };

    let duration = start.elapsed();
    let after_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    crate::metrics::record_metrics(crate::metrics::GcMetrics {
        duration,
        bytes_reclaimed: before_bytes.saturating_sub(after_bytes),
        bytes_surviving: after_bytes,
        objects_reclaimed,
        objects_surviving: N_EXISTING.with(Cell::get),
        collection_type,
        total_collections: 0,
    });

    crate::heap::resume_all_threads();
    crate::heap::clear_gc_request();

    // CRITICAL FIX: Clear global gc_in_progress flag after GC completes
    // This must be done AFTER resume_all_threads() so that new threads
    // don't see a false positive for in-progress GC.
    crate::heap::thread_registry()
        .lock()
        .unwrap()
        .set_gc_in_progress(false);

    IN_COLLECT.with(|in_collect| in_collect.set(false));
}

/// Perform a full garbage collection (Major GC).
///
/// This will collect all unreachable objects in both Young and Old generations.
/// Implements cooperative rendezvous for multi-threaded safety.
pub fn collect_full() {
    if IN_COLLECT.with(Cell::get) {
        return;
    }

    let is_collector = crate::heap::request_gc_handshake();

    if is_collector {
        perform_multi_threaded_collect_full();
    } else {
        // We're not the collector - wake up any threads waiting in rendezvous
        // and perform single-threaded collection
        crate::heap::GC_REQUESTED.store(false, Ordering::Relaxed);
        wake_waiting_threads();
        perform_single_threaded_collect_full();
    }
}

/// Wake up any threads waiting at a safe point and clear `gc_requested` for ALL threads.
/// This is used when a non-collector thread needs to wake up waiting threads
/// and perform single-threaded collection. It properly restores threads to
/// EXECUTING state and restores `active_count`.
///
/// CRITICAL: We must clear `gc_requested` for ALL threads, not just those at safepoint.
/// Otherwise, threads that haven't reached safepoint yet will have their flag stuck at true,
/// causing them to hang in future GC cycles when they enter rendezvous.
///
/// # Lock Ordering
///
/// Acquires `thread_registry()` lock (order 2). This function is called during
/// GC cleanup and must not be called while holding locks with order > 2.
///
/// # Safety
///
/// This function safely accesses the thread registry to wake threads that are
/// waiting at safepoints. The lock protects access to the thread list and their
/// state. After waking threads, it updates the `active_count` to reflect the
/// number of threads that have been resumed.
fn wake_waiting_threads() {
    let registry = crate::heap::thread_registry().lock().unwrap();
    let mut woken_count = 0;
    for tcb in &registry.threads {
        // Clear gc_requested for ALL threads to prevent hangs in future GC cycles
        tcb.gc_requested.store(false, Ordering::Release);

        if tcb.state.load(Ordering::Acquire) == crate::heap::THREAD_STATE_SAFEPOINT {
            tcb.park_cond.notify_all();
            tcb.state
                .store(crate::heap::THREAD_STATE_EXECUTING, Ordering::Release);
            woken_count += 1;
        }
    }
    registry
        .active_count
        .fetch_add(woken_count, std::sync::atomic::Ordering::SeqCst);
}

/// Perform single-threaded collection with atomic GC flag clearing and thread wake-up
/// to prevent race conditions where threads enter rendezvous after wake-up completes.
fn perform_single_threaded_collect_with_wake() {
    IN_COLLECT.with(|in_collect| in_collect.set(true));

    let start = std::time::Instant::now();
    let before_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    // Reset drop counter
    N_DROPS.with(|n| n.set(0));

    let mut objects_reclaimed = 0;
    let mut collection_type = crate::metrics::CollectionType::None;

    crate::heap::with_heap(|heap| {
        let total_size = heap.total_allocated();

        if total_size > MAJOR_THRESHOLD {
            collection_type = crate::metrics::CollectionType::Major;
            objects_reclaimed = collect_major(heap);
        } else {
            collection_type = crate::metrics::CollectionType::Minor;
            objects_reclaimed = collect_minor(heap);
        }
    });

    // Clear global flag before waking threads
    crate::heap::GC_REQUESTED.store(false, Ordering::SeqCst);

    // Wake threads AFTER collection completes to prevent concurrent access during GC
    {
        let registry = crate::heap::thread_registry().lock().unwrap();

        // Clear gc_requested for ALL threads to prevent deadlock
        // Threads that haven't reached safepoint yet will see gc_requested = false
        // and skip rendezvous entirely (safe since GC already completed)
        let mut woken_count = 0;
        for tcb in &registry.threads {
            tcb.gc_requested.store(false, Ordering::SeqCst);

            if tcb.state.load(Ordering::Acquire) == crate::heap::THREAD_STATE_SAFEPOINT {
                tcb.park_cond.notify_all();
                tcb.state
                    .store(crate::heap::THREAD_STATE_EXECUTING, Ordering::Release);
                woken_count += 1;
            }
        }

        // Restore active count for woken threads
        registry
            .active_count
            .fetch_add(woken_count, std::sync::atomic::Ordering::SeqCst);
    }

    let duration = start.elapsed();

    let after_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    // Record metrics
    let metrics = crate::metrics::GcMetrics {
        duration,
        bytes_reclaimed: before_bytes.saturating_sub(after_bytes),
        bytes_surviving: after_bytes,
        objects_reclaimed,
        objects_surviving: 0, // Could be calculated if needed
        collection_type,
        total_collections: 0, // Will be set by record_metrics
    };
    crate::metrics::record_metrics(metrics);

    IN_COLLECT.with(|in_collect| in_collect.set(false));
}

/// Perform single-threaded full collection (fallback for tests).
fn perform_single_threaded_collect_full() {
    #[cfg(feature = "tracing")]
    let gc_id = next_gc_id();
    #[cfg(feature = "tracing")]
    let _gc_span = trace_gc_collection("major_single_threaded", gc_id);

    IN_COLLECT.with(|in_collect| in_collect.set(true));

    let start = std::time::Instant::now();
    let before_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    let mut objects_reclaimed = 0;

    // Phase 1: Clear
    #[cfg(feature = "tracing")]
    let _clear_span = trace_phase(GcPhase::Clear);
    #[cfg(feature = "tracing")]
    log_phase_start(GcPhase::Clear, before_bytes);

    // Phase 2: Mark
    #[cfg(feature = "tracing")]
    let _mark_span = trace_phase(GcPhase::Mark);
    #[cfg(feature = "tracing")]
    log_phase_start(GcPhase::Mark, before_bytes);

    // Phase 3: Sweep
    #[cfg(feature = "tracing")]
    let _sweep_span = trace_phase(GcPhase::Sweep);
    #[cfg(feature = "tracing")]
    log_phase_start(GcPhase::Sweep, before_bytes);

    crate::heap::with_heap(|heap| {
        objects_reclaimed = collect_major(heap);
    });

    #[cfg(feature = "tracing")]
    log_phase_end(
        GcPhase::Sweep,
        before_bytes.saturating_sub(
            crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated()),
        ),
    );

    let duration = start.elapsed();

    let after_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    crate::metrics::record_metrics(crate::metrics::GcMetrics {
        duration,
        bytes_reclaimed: before_bytes.saturating_sub(after_bytes),
        bytes_surviving: after_bytes,
        objects_reclaimed,
        objects_surviving: N_EXISTING.with(Cell::get),
        collection_type: crate::metrics::CollectionType::Major,
        total_collections: 0,
    });

    IN_COLLECT.with(|in_collect| in_collect.set(false));
}

/// Perform full collection as the collector thread.
///
/// Uses the three-phase approach to correctly handle cross-heap references:
/// - Phase 1: Clear all marks on ALL heaps
/// - Phase 2: Mark all reachable objects (tracing across all heaps)
/// - Phase 3: Sweep ALL heaps
fn perform_multi_threaded_collect_full() {
    #[cfg(feature = "tracing")]
    let gc_id = next_gc_id();
    #[cfg(feature = "tracing")]
    let _gc_span = trace_gc_collection("major_multi_threaded", gc_id);

    IN_COLLECT.with(|in_collect| in_collect.set(true));

    let start = std::time::Instant::now();
    let before_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    // Reset drop counter
    N_DROPS.with(|n| n.set(0));

    let mut objects_reclaimed = 0;

    // CRITICAL FIX: Set global gc_in_progress flag BEFORE taking thread snapshot
    crate::heap::thread_registry()
        .lock()
        .unwrap()
        .set_gc_in_progress(true);

    // Collect all stack roots BEFORE processing heaps
    // This ensures we capture roots from all threads before any are consumed
    let tcbs = crate::heap::get_all_thread_control_blocks();
    let all_stack_roots: Vec<(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)> = tcbs
        .iter()
        .flat_map(|tcb| {
            let roots = crate::heap::take_stack_roots(tcb);
            roots.into_iter().map(move |ptr| (ptr, tcb.clone()))
        })
        .collect();

    // CRITICAL FIX: Use three-phase approach to correctly handle cross-heap references.
    // The old approach processed each heap independently, which caused marks on other
    // heaps (set during tracing of cross-heap references) to be cleared when processing
    // those heaps, leading to use-after-free bugs.

    // Phase 1: Clear all marks on ALL heaps
    for tcb in &tcbs {
        unsafe {
            clear_all_marks_and_dirty(&*tcb.heap.get());
        }
    }

    // Phase 2: Mark all reachable objects (tracing across all heaps)
    let mut total_objects_marked: usize = 0;
    for tcb in &tcbs {
        unsafe {
            total_objects_marked = total_objects_marked.saturating_add(mark_major_roots_multi(
                &mut *tcb.heap.get(),
                &all_stack_roots,
            ));
        }
    }

    // SAFETY: Fence ensures all mark bitmap writes are visible before sweeping.
    // This is the same race condition as perform_multi_threaded_collect.
    std::sync::atomic::fence(std::sync::atomic::Ordering::AcqRel);

    // Phase 3: Sweep ALL heaps
    for tcb in &tcbs {
        unsafe {
            let reclaimed = sweep_segment_pages(&*tcb.heap.get(), false);
            let reclaimed_large = sweep_large_objects(&mut *tcb.heap.get(), false);
            objects_reclaimed += reclaimed + reclaimed_large;
            promote_all_pages(&*tcb.heap.get());
        }
    }

    // Sweep orphan pages from terminated threads
    crate::heap::sweep_orphan_pages();

    let duration = start.elapsed();
    let after_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    crate::metrics::record_metrics(crate::metrics::GcMetrics {
        duration,
        bytes_reclaimed: before_bytes.saturating_sub(after_bytes),
        bytes_surviving: after_bytes,
        objects_reclaimed,
        objects_surviving: N_EXISTING.with(Cell::get),
        collection_type: crate::metrics::CollectionType::Major,
        total_collections: 0,
    });

    crate::heap::resume_all_threads();
    crate::heap::clear_gc_request();

    // CRITICAL FIX: Clear global gc_in_progress flag after GC completes
    crate::heap::thread_registry()
        .lock()
        .unwrap()
        .set_gc_in_progress(false);

    IN_COLLECT.with(|in_collect| in_collect.set(false));
}

/// Minor collection for a heap in multi-threaded context.
fn collect_minor_multi(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],
) -> usize {
    mark_minor_roots_multi(heap, stack_roots);
    let reclaimed = sweep_segment_pages(heap, true);
    let reclaimed_large = sweep_large_objects(heap, true);
    promote_young_pages(heap);
    reclaimed + reclaimed_large
}

/// Major collection for a heap in multi-threaded context.
///
/// Note: This function is currently unused because `perform_multi_threaded_collect_full()`
/// now uses the three-phase approach directly. Kept for potential future use.
#[allow(dead_code)]
fn collect_major_multi(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],
) -> usize {
    clear_all_marks_and_dirty(heap);
    let _objects_marked = mark_major_roots_multi(heap, stack_roots);
    let reclaimed = sweep_segment_pages(heap, false);
    let reclaimed_large = sweep_large_objects(heap, false);
    promote_all_pages(heap);
    reclaimed + reclaimed_large
}

/// Mark roots from all threads' stacks for Minor GC.
fn mark_minor_roots_multi(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],
) {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);

    for &(ptr, _) in stack_roots {
        unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                mark_object_minor(gc_box, &mut visitor);
            }
        }
    }

    unsafe {
        crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {
            if let Some(gc_box_ptr) =
                crate::heap::find_gc_box_from_ptr(heap, potential_ptr as *const u8)
            {
                mark_object_minor(gc_box_ptr, &mut visitor);
            }
        });
    }

    TEST_ROOTS.with(|roots| {
        for &ptr in roots.borrow().iter() {
            unsafe {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                    mark_object_minor(gc_box, &mut visitor);
                }
            }
        }
    });

    for (_, tcb) in stack_roots {
        tcb.iterate_all_handles(|ptr| unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr.cast::<u8>()) {
                mark_object_minor(gc_box, &mut visitor);
            }
        });
    }

    #[cfg(feature = "tokio")]
    #[allow(clippy::explicit_iter_loop)]
    {
        use crate::tokio::GcRootSet;
        for &ptr in GcRootSet::global().snapshot(heap).iter() {
            unsafe {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                    mark_object_minor(gc_box, &mut visitor);
                }
            }
        }
    }

    // Take snapshot of dirty pages for lock-free scanning
    let _dirty_count = heap.take_dirty_pages_snapshot();

    // Scan ONLY dirty pages (not all pages)
    for page_ptr in heap.dirty_pages_iter() {
        unsafe {
            let header = page_ptr.as_ptr();
            // Defensive: skip young pages (shouldn't happen)
            if (*header).generation == 0 {
                continue;
            }
            if (*header).is_large_object() {
                let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                mark_and_trace_incremental(
                    std::ptr::NonNull::new_unchecked(gc_box_ptr),
                    &mut visitor,
                );
            } else {
                let obj_count = (*header).obj_count as usize;
                for i in 0..obj_count {
                    if (*header).is_dirty(i) {
                        let block_size = (*header).block_size as usize;
                        let header_size = PageHeader::header_size(block_size);
                        let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));
                        #[allow(clippy::cast_ptr_alignment)]
                        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                        mark_and_trace_incremental(
                            std::ptr::NonNull::new_unchecked(gc_box_ptr),
                            &mut visitor,
                        );
                    }
                }
            }
            // Clear dirty state after scanning
            (*header).clear_all_dirty();
            (*header).clear_dirty_listed();
        }
    }

    // Clear snapshot and update statistics
    heap.clear_dirty_pages_snapshot();

    while let Some(ptr) = visitor.worklist.pop() {
        unsafe {
            ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), &mut visitor);
        }
    }
}

#[inline]
unsafe fn mark_and_push_to_worker_queue(
    ptr: *const u8,
    gc_box: NonNull<GcBox<()>>,
    worker_queues: &[PerThreadMarkQueue],
    num_workers: usize,
) {
    unsafe {
        let ptr_addr = gc_box.as_ptr() as *const u8;
        let header = crate::heap::ptr_to_page_header(ptr_addr);
        if (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE {
            if let Some(idx) = crate::heap::ptr_to_object_index(gc_box.as_ptr().cast()) {
                if !(*header.as_ptr()).is_marked(idx) {
                    (*header.as_ptr()).set_mark(idx);
                }
            }
        }
        let worker_idx = ptr as usize % num_workers;
        worker_queues[worker_idx].push(gc_box.as_ptr());
    }
}

/// Mark roots using parallel marking for Minor GC.
///
/// This function processes dirty pages in parallel, distributing them
/// across worker queues based on page ownership.
#[allow(dead_code)]
#[allow(
    clippy::unnecessary_cast,
    clippy::ptr_cast_constness,
    clippy::too_many_lines
)]
fn mark_minor_roots_parallel(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],
    config: ParallelMarkConfig,
) {
    if config.max_workers < 2 || !config.parallel_minor_gc {
        mark_minor_roots_multi(heap, stack_roots);
        return;
    }

    let num_workers = config
        .max_workers
        .min(crate::gc::marker::available_parallelism());

    let worker_queues = crate::gc::marker::create_worker_queues(num_workers);

    let _visitor = GcVisitor::new(VisitorKind::Minor);

    for &(ptr, _) in stack_roots.iter().take(num_workers) {
        unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                mark_and_push_to_worker_queue(ptr, gc_box, &worker_queues, num_workers);
            }
        }
    }

    unsafe {
        crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {
            if let Some(gc_box_ptr) =
                crate::heap::find_gc_box_from_ptr(heap, potential_ptr as *const u8)
            {
                mark_and_push_to_worker_queue(
                    potential_ptr as *const u8,
                    gc_box_ptr,
                    &worker_queues,
                    num_workers,
                );
            }
        });
    }

    TEST_ROOTS.with(|roots| {
        for &ptr in roots.borrow().iter() {
            unsafe {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                    mark_and_push_to_worker_queue(ptr, gc_box, &worker_queues, num_workers);
                }
            }
        }
    });

    #[cfg(feature = "tokio")]
    #[allow(clippy::explicit_iter_loop)]
    {
        use crate::tokio::GcRootSet;
        for &ptr in GcRootSet::global().snapshot(heap).iter() {
            unsafe {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                    mark_and_push_to_worker_queue(
                        ptr as *const u8,
                        gc_box,
                        &worker_queues,
                        num_workers,
                    );
                }
            }
        }
    }

    // Take snapshot of dirty pages for lock-free scanning
    let _dirty_count = heap.take_dirty_pages_snapshot();

    // Collect dirty pages into a local vector for parallel distribution
    let mut dirty_pages: Vec<*const PageHeader> = Vec::new();
    for page_ptr in heap.dirty_pages_iter() {
        unsafe {
            let header = page_ptr.as_ptr();
            // Defensive: skip young pages (shouldn't happen)
            if (*header).generation == 0 {
                continue;
            }
            // Handle large objects: add to work queue for parallel marking
            if (*header).is_large_object() {
                let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                // Add to first worker queue (will be distributed by work stealing)
                worker_queues[0].push(gc_box_ptr);
                continue; // Large objects don't use per-object dirty tracking
            }
            dirty_pages.push(header);
        }
    }

    let distribution = crate::gc::marker::distribute_dirty_pages(&dirty_pages, &worker_queues);

    for (idx, page) in dirty_pages.iter().enumerate() {
        let worker_idx = distribution[idx];
        unsafe {
            let header = *page;
            let obj_count = (*header).obj_count as usize;
            for i in 0..obj_count {
                if (*header).is_dirty(i) {
                    let block_size = (*header).block_size as usize;
                    let header_size = PageHeader::header_size(block_size);
                    let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                    worker_queues[worker_idx].push(gc_box_ptr);
                }
            }
        }
    }

    let all_queues: Vec<Arc<PerThreadMarkQueue>> =
        worker_queues.into_iter().map(Arc::new).collect();
    let num_queues = all_queues.len();

    let registry = GcWorkerRegistry::new(num_queues);

    let mut handles = Vec::new();
    for i in 0..num_queues {
        let queues = all_queues.clone();
        let queue = all_queues[i].clone();
        let registry = registry.clone();
        let handle = std::thread::spawn(move || {
            worker_mark_loop_with_registry(queue, &registry, &queues, VisitorKind::Minor)
        });
        handles.push(handle);
    }

    registry.notify_work_available();

    for handle in handles {
        let _ = handle.join().unwrap();
    }

    registry.set_complete();

    // Clear dirty state for all pages in the snapshot (using already-collected vector)
    clear_dirty_page_states(&dirty_pages);

    // Clear snapshot and update statistics
    heap.clear_dirty_pages_snapshot();

    crate::gc::marker::clear_overflow_queue();
}

/// Mark roots from all threads' stacks for Major GC.
/// Returns the number of objects marked.
fn mark_major_roots_multi(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],
) -> usize {
    let mut visitor = GcVisitor::new(VisitorKind::Major);

    for &(ptr, _) in stack_roots {
        unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                mark_object(gc_box, &mut visitor);
            }
        }
    }

    unsafe {
        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                mark_object(gc_box, &mut visitor);
            }
        });
    }

    TEST_ROOTS.with(|roots| {
        for &ptr in roots.borrow().iter() {
            unsafe {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                    mark_object(gc_box, &mut visitor);
                }
            }
        }
    });

    for (_, tcb) in stack_roots {
        tcb.iterate_all_handles(|ptr| unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr.cast::<u8>()) {
                mark_object(gc_box, &mut visitor);
            }
        });
    }

    #[cfg(feature = "tokio")]
    #[allow(clippy::explicit_iter_loop)]
    {
        use crate::tokio::GcRootSet;
        for &ptr in GcRootSet::global().snapshot(heap).iter() {
            unsafe {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                    mark_object(gc_box, &mut visitor);
                }
            }
        }
    }

    while let Some(ptr) = visitor.worklist.pop() {
        unsafe {
            ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), &mut visitor);
        }
    }

    visitor.objects_marked()
}

/// Mark roots using parallel marking with work stealing.
///
/// This function sets up parallel marking infrastructure and distributes
/// root objects across worker queues. Workers process their queues in parallel,
/// with work stealing to balance load.
#[allow(dead_code)]
#[allow(clippy::unnecessary_cast, clippy::ptr_cast_constness)]
fn mark_major_roots_parallel(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],
    config: ParallelMarkConfig,
) -> usize {
    if config.max_workers < 2 || !config.parallel_major_gc {
        return mark_major_roots_multi(heap, stack_roots);
    }
    #[cfg(feature = "tracing")]
    let _span = trace_phase(GcPhase::Mark);
    let mut total_marked: usize = 0;

    let num_workers = config
        .max_workers
        .min(crate::gc::marker::available_parallelism());

    let worker_queues = crate::gc::marker::create_worker_queues(num_workers);

    let root_pages: Vec<*const PageHeader> = heap
        .all_pages()
        .filter_map(|p| unsafe {
            let header = p.as_ptr();
            if (*header).generation == 1 {
                Some(header.cast_const())
            } else {
                None
            }
        })
        .collect();

    let _visitor = GcVisitor::new(VisitorKind::Major);

    for &(ptr, _) in stack_roots.iter().take(root_pages.len().min(num_workers)) {
        unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                mark_and_push_to_worker_queue(ptr, gc_box, &worker_queues, num_workers);
            }
        }
    }

    unsafe {
        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                mark_and_push_to_worker_queue(
                    ptr as *const u8,
                    gc_box,
                    &worker_queues,
                    num_workers,
                );
            }
        });
    }

    TEST_ROOTS.with(|roots| {
        for &ptr in roots.borrow().iter() {
            unsafe {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                    mark_and_push_to_worker_queue(ptr, gc_box, &worker_queues, num_workers);
                }
            }
        }
    });

    let all_queues: Vec<Arc<PerThreadMarkQueue>> =
        worker_queues.into_iter().map(Arc::new).collect();
    let num_queues = all_queues.len();

    let registry = GcWorkerRegistry::new(num_queues);

    let mut handles = Vec::new();
    for i in 0..num_queues {
        let queues = all_queues.clone();
        let queue = all_queues[i].clone();
        let registry = registry.clone();
        let handle = std::thread::spawn(move || {
            worker_mark_loop_with_registry(queue, &registry, &queues, VisitorKind::Major)
        });
        handles.push(handle);
    }

    registry.notify_work_available();

    for handle in handles {
        let marked = handle.join().unwrap();
        total_marked = total_marked.saturating_add(marked);
    }

    registry.set_complete();

    crate::gc::marker::clear_overflow_queue();

    #[cfg(feature = "tracing")]
    crate::gc::tracing::log_parallel_mark_stats(num_workers, total_marked);

    total_marked
}

/// Minor Collection: Collect Young Generation only.
///
/// # FR-009 Compliance
///
/// Per spec requirement FR-009: "System MUST prevent minor GC from running during
/// incremental major marking." This implementation blocks until incremental major
/// marking completes rather than skipping minor GC entirely.
fn collect_minor(heap: &mut LocalHeap) -> usize {
    #[cfg(feature = "tracing")]
    let gc_id = next_gc_id();
    #[cfg(feature = "tracing")]
    let _gc_span = trace_gc_collection("minor", gc_id);

    if crate::gc::incremental::is_incremental_marking_active() {
        crate::heap::wait_for_gc_complete();
    }

    let before_bytes = heap.total_allocated();

    // 1. Mark Phase
    #[cfg(feature = "tracing")]
    let _mark_span = trace_phase(GcPhase::Mark);
    #[cfg(feature = "tracing")]
    log_phase_start(GcPhase::Mark, before_bytes);

    let objects_marked = mark_minor_roots(heap);

    #[cfg(feature = "tracing")]
    log_phase_end_mark(GcPhase::Mark, objects_marked);

    // 2. Sweep Phase
    #[cfg(feature = "tracing")]
    let _sweep_span = trace_phase(GcPhase::Sweep);
    #[cfg(feature = "tracing")]
    log_phase_start(GcPhase::Sweep, before_bytes);

    let reclaimed = sweep_segment_pages(heap, true);
    let reclaimed_large = sweep_large_objects(heap, true);

    #[cfg(feature = "tracing")]
    log_phase_end(GcPhase::Sweep, reclaimed + reclaimed_large);

    // 3. Promotion Phase
    promote_young_pages(heap);
    reclaimed + reclaimed_large
}

/// Promote Young Pages to Old Generation.
fn promote_young_pages(heap: &mut LocalHeap) {
    let mut promoted_bytes = 0;

    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            if (*header).generation == 0 {
                // Determine if page has survivors
                let mut has_survivors = false;
                let mut survivors_count = 0;

                for i in 0..crate::heap::BITMAP_SIZE {
                    let bits = (*header).allocated_bitmap[i].load(Ordering::Acquire);
                    if bits != 0 {
                        has_survivors = true;
                        survivors_count += bits.count_ones() as usize;
                    }
                }

                if has_survivors {
                    (*header).generation = 1; // Promote!

                    let block_size = (*header).block_size as usize;
                    promoted_bytes += survivors_count * block_size;
                }
            }
        }
    }

    // Update GlobalHeap stats
    // After Minor GC, all small young objects are either promoted or swept.
    // So young generation usage for small objects is effectively 0.
    let old = heap.old_allocated();
    heap.update_allocated_bytes(0, old + promoted_bytes);
}

/// Major Collection: Collect Entire Heap.
///
/// # Design Note
///
/// The spec's data model (specs/008-incremental-marking/data-model.md section 2.4) defines
/// a `GcRequest` struct with `CollectionType::IncrementalMajor` as the trigger mechanism.
/// This implementation uses a different approach:
///
/// 1. The `gc_requested: AtomicBool` flag in `ThreadControlBlock` serves as the GC trigger
/// 2. `IncrementalConfig::enabled` controls whether incremental marking is used
/// 3. `CollectionType::IncrementalMajor` in `metrics.rs` records what *happened* for telemetry
///
/// This avoids introducing a `GcRequest` struct when the existing flag-based approach works.
fn collect_major(heap: &mut LocalHeap) -> usize {
    let config = crate::gc::incremental::IncrementalMarkState::global().config();

    if config.enabled {
        collect_major_incremental(heap)
    } else {
        collect_major_stw(heap)
    }
}

fn collect_major_stw(heap: &mut LocalHeap) -> usize {
    clear_all_marks_and_dirty(heap);
    mark_major_roots(heap);

    let reclaimed = sweep_segment_pages(heap, false);
    let reclaimed_large = sweep_large_objects(heap, false);

    promote_all_pages(heap);

    reclaimed + reclaimed_large
}

#[allow(clippy::significant_drop_tightening)]
fn collect_major_incremental(heap: &mut LocalHeap) -> usize {
    let state = IncrementalMarkState::global();
    let config = state.config();

    let heaps: [&LocalHeap; 1] = [&*heap];
    execute_snapshot(&heaps);

    let per_worker_budget = config.increment_size;

    loop {
        let result = mark_slice(heap, per_worker_budget);

        match result {
            MarkSliceResult::Complete { .. } => {
                break;
            }
            MarkSliceResult::Pending { .. } => {}
            MarkSliceResult::Fallback { reason } => {
                log_fallback_reason(reason);
                state.set_phase(MarkPhase::FinalMark);
                break;
            }
        }
    }

    let remaining = state.worklist_len();
    let dirty_pages = count_dirty_pages(heap);
    if remaining > 0 || dirty_pages > 0 {
        let heaps_mut: &mut [&mut LocalHeap; 1] = &mut [heap];
        execute_final_mark(heaps_mut);
    }

    state.set_phase(MarkPhase::Sweeping);

    let reclaimed = sweep_segment_pages(heap, false);
    let reclaimed_large = sweep_large_objects(heap, false);

    promote_all_pages(heap);

    state.set_phase(MarkPhase::Idle);

    reclaimed + reclaimed_large
}

/// Clear all mark bits, dirty bits, and reset `dead_count` in the heap.
///
/// # Invariants
///
/// - `PAGE_FLAG_DIRTY_LISTED` is NOT cleared here because:
///   - Minor GC clears it via `clear_dirty_page_states()` after scanning dirty pages
///   - Major GC clears it via `clear_dirty_page_states()` after processing its dirty page list
///   - These two GC types are never nested, so the invariant holds
///
/// This separation ensures dirty page tracking works correctly across GC cycles.
fn clear_all_marks_and_dirty(heap: &LocalHeap) {
    for page_ptr in heap.all_pages() {
        // SAFETY: Page pointers in the heap are always valid
        unsafe {
            let header = page_ptr.as_ptr();
            (*header).clear_all_marks();
            (*header).clear_all_dirty();
            #[cfg(feature = "lazy-sweep")]
            (*header).set_dead_count(0);
        }
    }
}

#[inline]
fn clear_dirty_page_states(headers: &[*const PageHeader]) {
    for &header_ptr in headers {
        unsafe {
            let header_mut = header_ptr.cast_mut();
            (*header_mut).clear_all_dirty();
            (*header_mut).clear_dirty_listed();
        }
    }
}

/// Mark roots for Minor GC (Stack + `RemSet`).
/// Optimized to scan only dirty pages instead of all pages.
/// Returns the number of objects marked.
fn mark_minor_roots(heap: &mut LocalHeap) -> usize {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);

    unsafe {
        crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {
            if let Some(gc_box_ptr) =
                crate::heap::find_gc_box_from_ptr(heap, potential_ptr as *const u8)
            {
                mark_object_minor(gc_box_ptr, &mut visitor);
            }
        });

        #[cfg(any(test, feature = "test-util"))]
        TEST_ROOTS.with(|roots| {
            for &ptr in roots.borrow().iter() {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                    mark_object_minor(gc_box, &mut visitor);
                }
            }
        });
    }

    // Take snapshot of dirty pages for lock-free scanning
    let _dirty_count = heap.take_dirty_pages_snapshot();

    // Scan ONLY dirty pages (not all pages)
    for page_ptr in heap.dirty_pages_iter() {
        unsafe {
            let header = page_ptr.as_ptr();
            // Defensive: skip young pages (shouldn't happen)
            if (*header).generation == 0 {
                continue;
            }
            if (*header).is_large_object() {
                let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
            } else {
                let obj_count = (*header).obj_count as usize;
                for i in 0..obj_count {
                    if (*header).is_dirty(i) {
                        let block_size = (*header).block_size as usize;
                        let header_size = PageHeader::header_size(block_size);
                        let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));
                        #[allow(clippy::cast_ptr_alignment)]
                        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                        ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
                    }
                }
            }
            // Clear dirty state after scanning
            (*header).clear_all_dirty();
            (*header).clear_dirty_listed();
        }
    }

    // Clear snapshot and update statistics
    heap.clear_dirty_pages_snapshot();

    visitor.process_worklist();
    visitor.objects_marked()
}

/// Mark roots for Major GC (Stack).
/// Returns the number of objects marked.
fn mark_major_roots(heap: &LocalHeap) -> usize {
    let mut visitor = GcVisitor::new(VisitorKind::Major);
    unsafe {
        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                mark_object(gc_box, &mut visitor);
            }
        });

        #[cfg(any(test, feature = "test-util"))]
        TEST_ROOTS.with(|roots| {
            for &ptr in roots.borrow().iter() {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                    mark_object(gc_box, &mut visitor);
                }
            }
        });
    }
    visitor.process_worklist();
    visitor.objects_marked()
}

/// Mark object for Minor GC - adds to worklist for iterative tracing.
///
/// # Safety
///
/// The pointer must be a valid, non-null `GcBox` pointer that was previously
/// returned by `Gc::allocate()`. The visitor must be a valid `GcVisitor`
/// instance.
pub unsafe fn mark_object_minor(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let page_addr = (ptr_addr as usize) & crate::heap::page_mask();
    let header = unsafe { crate::heap::ptr_to_page_header(ptr_addr) };

    unsafe {
        if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
            return;
        }

        if (*header.as_ptr()).generation > 0 {
            return;
        }

        let block_size = (*header.as_ptr()).block_size as usize;
        let header_size = PageHeader::header_size(block_size);
        let data_start = page_addr + header_size;
        let offset = ptr_addr as usize - data_start;
        let index = offset / block_size;

        if (*header.as_ptr()).is_marked(index) {
            return;
        }

        (*header.as_ptr()).set_mark(index);
        visitor.objects_marked += 1;

        visitor.worklist.push(ptr);
    }
}

/// Sweep pages in regular segments.
///
/// Two-phase sweep to prevent Use-After-Free during Drop:
/// - Phase 1: Execute all Drop functions (objects still accessible)
/// - Phase 2: Reclaim memory and rebuild free lists
fn sweep_segment_pages(heap: &LocalHeap, only_young: bool) -> usize {
    #[cfg(feature = "tracing")]
    tracing::debug!(heap_bytes = heap.total_allocated(), "sweep_start");

    let pending = sweep_phase1_finalize(heap, only_young);
    let reclaimed = sweep_phase2_reclaim(heap, pending, only_young);

    #[cfg(feature = "tracing")]
    tracing::debug!(objects_freed = reclaimed, "sweep_end");

    reclaimed
}

/// Phase 1: Execute Drop functions for all dead objects.
///
/// This phase only calls `drop_fn` but does NOT reclaim memory yet.
/// This ensures that during Drop, all other GC objects are still accessible.
fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {
    let mut pending = Vec::new();

    // Snapshot pages to prevent iterator invalidation if drop_fn allocates memory
    // (which could trigger heap.pages.push() and invalidate the iterator)
    let pages_snapshot: Vec<_> = heap.all_pages().collect();

    for page_ptr in pages_snapshot {
        unsafe {
            let header = page_ptr.as_ptr();

            if (*header).is_large_object() {
                continue;
            }

            if only_young && (*header).generation > 0 {
                continue;
            }

            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);

            for i in 0..obj_count {
                if (*header).is_marked(i) {
                    // Object is reachable - clear mark for next collection
                    (*header).clear_mark(i);
                } else if (*header).is_allocated(i) {
                    // Object is unreachable but allocated - needs cleanup
                    let obj_ptr = page_ptr.as_ptr().cast::<u8>();
                    let obj_ptr = obj_ptr.add(header_size + i * block_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                    let weak_count = (*gc_box_ptr).weak_count();

                    if weak_count > 0 {
                        // Has weak refs - drop value but keep allocation
                        if !(*gc_box_ptr).is_value_dead() {
                            ((*gc_box_ptr).drop_fn)(obj_ptr);
                            (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                            (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                            (*gc_box_ptr).set_dead();
                        }
                    } else {
                        // No weak refs - will be fully reclaimed
                        // Execute drop_fn now (phase 1)
                        ((*gc_box_ptr).drop_fn)(obj_ptr);

                        // CRITICAL FIX: Mark as dead so phase 2 knows to reclaim.
                        // Without this, is_value_dead() returns false in phase 2,
                        // objects are never reclaimed, and the next GC cycle will
                        // try to drop them again - use-after-free!
                        (*gc_box_ptr).set_dead();

                        pending.push(PendingDrop {
                            page: page_ptr,
                            index: i,
                        });
                    }
                }
            }
        }
    }

    pending
}

/// Phase 2: Reclaim memory and rebuild free lists.
///
/// This phase runs AFTER all Drop functions have completed,
/// so it's safe to reclaim memory.
///
/// Optimized: Uses bitmap checks instead of `PendingDrop` tracking
/// to eliminate `HashMap` overhead and reduce GC pause time.
#[allow(
    clippy::branches_sharing_code,
    clippy::if_not_else,
    clippy::doc_markdown
)]
fn sweep_phase2_reclaim(heap: &LocalHeap, _pending: Vec<PendingDrop>, only_young: bool) -> usize {
    let mut reclaimed = 0;

    // Process each page: rebuild free list from scratch
    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();

            if (*header).is_large_object() {
                continue;
            }

            if only_young && (*header).generation > 0 {
                continue;
            }

            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);
            let page_addr = header.cast::<u8>();

            // Rebuild free list from scratch (iterate in reverse for correct allocation order)
            let mut free_head: Option<u16> = None;
            for i in (0..obj_count).rev() {
                let mut is_alloc = (*header).is_allocated(i);
                let is_marked = (*header).is_marked(i);

                if is_alloc && !is_marked {
                    // Slot is allocated but not marked - candidate for reclamation
                    let obj_ptr = page_addr.add(header_size + i * block_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                    let weak_count = (*gc_box_ptr).weak_count();

                    if weak_count == 0 && (*gc_box_ptr).is_value_dead() {
                        // No weak refs, already dropped and dead - reclaim
                        // CRITICAL FIX: Write free list head BEFORE clearing allocated bit
                        // to prevent new allocations from reusing this slot with corrupted metadata
                        #[allow(clippy::cast_ptr_alignment)]
                        let obj_cast = obj_ptr.cast::<Option<u16>>();
                        obj_cast.write_unaligned(free_head);
                        free_head = Some(u16::try_from(i).unwrap());

                        (*header).clear_allocated(i);
                        reclaimed += 1;
                        is_alloc = false;
                        continue;
                    }
                }

                if !is_alloc {
                    // Slot is free - add to free list (if not already done above)
                    let obj_ptr = page_addr.add(header_size + i * block_size);
                    // Check if free list head was already written (for reclaimed slots)
                    let current_head = (*header).free_list_head();
                    let idx_as_u16 = u16::try_from(i).unwrap();
                    let head_written = current_head == Some(idx_as_u16);

                    if !head_written {
                        #[allow(clippy::cast_ptr_alignment)]
                        let obj_cast = obj_ptr.cast::<Option<u16>>();
                        obj_cast.write_unaligned(free_head);
                        free_head = Some(u16::try_from(i).unwrap());
                    }
                }
            }
            (*header).set_free_list_head(free_head);
        }
    }

    // NOTE: We do NOT decrement N_EXISTING here.
    // N_EXISTING tracks total allocated objects for GC heuristics.
    // Decrementing during sweep causes the heuristic to think heap is nearly empty,
    // triggering unnecessary GC cycles during the subsequent Drop phase.
    // Objects are counted at allocation time only; N_EXISTING reflects live objects.

    reclaimed
}

/// Promote ALL pages (after Major GC).
fn promote_all_pages(heap: &LocalHeap) {
    for page_ptr in heap.all_pages() {
        unsafe {
            (*page_ptr.as_ptr()).generation = 1;
        }
    }
}

/// Mark a single object and add to worklist for iterative tracing.
///
/// # Safety
///
/// The pointer must be a valid `GcBox` pointer.
pub unsafe fn mark_object(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let header = unsafe { crate::heap::ptr_to_page_header(ptr_addr) };

    unsafe {
        if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
            return;
        }

        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
            if (*header.as_ptr()).is_marked(idx) {
                return;
            }
            (*header.as_ptr()).set_mark(idx);
            visitor.objects_marked += 1;
        } else {
            return;
        }

        visitor.worklist.push(ptr);
    }
}

#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn mark_and_trace_incremental(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let header = crate::heap::ptr_to_page_header(ptr_addr);

    if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
        return;
    }

    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
        if visitor.kind == VisitorKind::Minor && (*header.as_ptr()).generation > 0 {
            return;
        }

        if (*header.as_ptr()).is_marked(idx) {
            return;
        }
        (*header.as_ptr()).set_mark(idx);
    } else {
        return;
    }

    visitor.worklist.push(ptr);
}

/// Sweep Large Object Space.
///
/// Large objects that are unmarked should be deallocated entirely.
fn sweep_large_objects(heap: &mut LocalHeap, only_young: bool) -> usize {
    let target_pages = heap.large_object_pages();

    let mut to_deallocate: Vec<(NonNull<PageHeader>, usize, usize)> = Vec::new();

    for page_ptr in target_pages {
        unsafe {
            let header = page_ptr.as_ptr();

            if only_young && (*header).generation > 0 {
                continue;
            }

            if !(*header).is_marked(0) {
                let block_size = (*header).block_size as usize;
                let header_size = (*header).header_size as usize;
                let obj_ptr = header.cast::<u8>().add(header_size);
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                let weak_count = (*gc_box_ptr).weak_count();

                if weak_count > 0 {
                    if !(*gc_box_ptr).is_value_dead() {
                        ((*gc_box_ptr).drop_fn)(obj_ptr);
                        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                        (*gc_box_ptr).set_dead();
                    }
                } else {
                    let total_size = header_size + block_size;
                    let pages_needed = total_size.div_ceil(crate::heap::page_size());
                    let alloc_size = pages_needed * crate::heap::page_size();

                    ((*gc_box_ptr).drop_fn)(obj_ptr);

                    to_deallocate.push((page_ptr, alloc_size, pages_needed));
                }
            }
        }
    }

    let mut reclaimed = 0;

    // Batch collect pages to remove for O(N) instead of O(N²)
    let pages_to_remove: HashSet<usize> = to_deallocate
        .iter()
        .map(|(page_ptr, _, _)| page_ptr.as_ptr() as usize)
        .collect();

    for (page_ptr, alloc_size, pages_needed) in to_deallocate {
        unsafe {
            let header_addr = page_ptr.as_ptr() as usize;

            // Remove from both maps. If a panic occurs between operations,
            // the state may be temporarily inconsistent, but the page will
            // still be deallocated. In practice, panics during GC are catastrophic.
            for p in 0..pages_needed {
                let page_addr = header_addr + (p * crate::heap::page_size());
                heap.large_object_map.remove(&page_addr);
            }
            {
                let mut manager = crate::heap::segment_manager()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                for p in 0..pages_needed {
                    let page_addr = header_addr + (p * crate::heap::page_size());
                    manager.large_object_map.remove(&page_addr);
                }
            }

            sys_alloc::Mmap::from_raw(page_ptr.as_ptr().cast::<u8>(), alloc_size);

            reclaimed += 1;
        }
    }

    // Batch remove pages from heap.pages (O(N) instead of O(N²))
    heap.pages
        .retain(|&p| !pages_to_remove.contains(&(p.as_ptr() as usize)));

    // NOTE: We do NOT decrement N_EXISTING here.
    // See sweep_phase2_reclaim for explanation.

    reclaimed
}

// ============================================================================
// Lazy Sweep - Incremental sweep during allocation
// ============================================================================

#[cfg(feature = "lazy-sweep")]
#[allow(unsafe_op_in_unsafe_fn)]
/// Performs lazy sweep on a single page, reclaiming all dead objects.
///
/// # Safety
///
/// - `page_ptr` must point to a valid `PageHeader` for a page that needs sweeping
/// - `block_size` must match the page's object size class
/// - `obj_count` must match the page's maximum object count
/// - `header_size` must be correctly calculated for the block size
/// - The page must not be concurrently accessed by other threads during sweep
/// - Caller must ensure no new allocations occur on this page during sweep
unsafe fn lazy_sweep_page(
    page_ptr: NonNull<PageHeader>,
    block_size: usize,
    obj_count: usize,
    header_size: usize,
) -> (usize, bool) {
    let header = page_ptr.as_ptr();
    let page_addr = page_ptr.as_ptr().cast::<u8>();
    let mut reclaimed = 0;
    let mut all_dead = true;

    for i in 0..obj_count {
        let is_allocated = (*header).is_allocated(i);
        let is_marked = (*header).is_marked(i);

        if is_allocated && !is_marked {
            let obj_ptr = page_addr.add(header_size + i * block_size);
            #[allow(clippy::cast_ptr_alignment)]
            let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

            let weak_count = (*gc_box_ptr).weak_count();

            if weak_count > 0 {
                if !(*gc_box_ptr).is_value_dead() {
                    ((*gc_box_ptr).drop_fn)(obj_ptr);
                    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                    (*gc_box_ptr).set_dead();
                }
                all_dead = false;
            } else {
                ((*gc_box_ptr).drop_fn)(obj_ptr);
                (*gc_box_ptr).set_dead();

                #[allow(clippy::cast_ptr_alignment)]
                let obj_cast = obj_ptr.cast::<Option<u16>>();
                let mut current_free = (*header).free_list_head();
                obj_cast.write_unaligned(current_free);
                loop {
                    let old = current_free.unwrap_or(u16::MAX);
                    match (*header).free_list_head.compare_exchange(
                        old,
                        u16::try_from(i).unwrap(),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            if (*header).is_allocated(i) {
                                let next_head = current_free;
                                if (*header)
                                    .free_list_head
                                    .compare_exchange(
                                        u16::try_from(i).unwrap(),
                                        old,
                                        Ordering::AcqRel,
                                        Ordering::Acquire,
                                    )
                                    .is_err()
                                {
                                    current_free = (*header).free_list_head();
                                    if current_free == Some(u16::try_from(i).unwrap()) {
                                        let next = obj_cast.read_unaligned();
                                        let _ = (*header).free_list_head.compare_exchange(
                                            u16::try_from(i).unwrap(),
                                            next.unwrap_or(u16::MAX),
                                            Ordering::AcqRel,
                                            Ordering::Acquire,
                                        );
                                    }
                                    current_free = if (*header).free_list_head()
                                        == Some(u16::try_from(i).unwrap())
                                    {
                                        None
                                    } else {
                                        (*header).free_list_head()
                                    };
                                    obj_cast.write_unaligned(current_free);
                                } else {
                                    current_free = next_head;
                                }
                                continue;
                            }
                            break;
                        }
                        Err(actual) => {
                            current_free = if actual == u16::MAX {
                                None
                            } else {
                                Some(actual)
                            };
                            obj_cast.write_unaligned(current_free);
                        }
                    }
                }

                (*header).clear_allocated(i);
                reclaimed += 1;
            }
        } else {
            (*header).clear_mark(i);
            all_dead = false;
        }
    }

    (reclaimed, all_dead)
}

#[cfg(feature = "lazy-sweep")]
#[allow(unsafe_op_in_unsafe_fn)]
/// Fast-path sweep for pages where all objects are dead.
///
/// # Safety
///
/// - `page_ptr` must point to a valid `PageHeader` for a page with all-dead objects
/// - `block_size` must match the page's object size class
/// - `obj_count` must match the page's maximum object count
/// - `header_size` must be correctly calculated for the block size
/// - The `PAGE_FLAG_ALL_DEAD` flag must be set on this page
/// - The page must not be concurrently accessed by other threads during sweep
/// - Caller must ensure no new allocations occur on this page during sweep
/// - After this function returns, the caller is responsible for:
///   - Clearing `PAGE_FLAG_ALL_DEAD` (via `clear_all_dead()`)
///   - Clearing `PAGE_FLAG_NEEDS_SWEEP` (via `clear_needs_sweep()`)
///   - Resetting `dead_count` to 0 (via `set_dead_count(0)`)
unsafe fn lazy_sweep_page_all_dead(
    page_ptr: NonNull<PageHeader>,
    block_size: usize,
    obj_count: usize,
    header_size: usize,
) -> usize {
    let header = page_ptr.as_ptr();
    let page_addr = page_ptr.as_ptr().cast::<u8>();
    let mut reclaimed = 0;

    for i in 0..obj_count {
        if (*header).is_allocated(i) {
            let obj_ptr = page_addr.add(header_size + i * block_size);
            #[allow(clippy::cast_ptr_alignment)]
            let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

            let weak_count = (*gc_box_ptr).weak_count();

            if weak_count > 0 {
                if !(*gc_box_ptr).is_value_dead() {
                    ((*gc_box_ptr).drop_fn)(obj_ptr);
                    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                    (*gc_box_ptr).set_dead();
                }
            } else {
                ((*gc_box_ptr).drop_fn)(obj_ptr);

                #[allow(clippy::cast_ptr_alignment)]
                let obj_cast = obj_ptr.cast::<Option<u16>>();
                let mut current_free = (*header).free_list_head();
                obj_cast.write_unaligned(current_free);
                loop {
                    let old = current_free.unwrap_or(u16::MAX);
                    match (*header).free_list_head.compare_exchange(
                        old,
                        u16::try_from(i).unwrap(),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            if (*header).is_allocated(i) {
                                let next_head = current_free;
                                if (*header)
                                    .free_list_head
                                    .compare_exchange(
                                        u16::try_from(i).unwrap(),
                                        old,
                                        Ordering::AcqRel,
                                        Ordering::Acquire,
                                    )
                                    .is_err()
                                {
                                    current_free = (*header).free_list_head();
                                    if current_free == Some(u16::try_from(i).unwrap()) {
                                        let next = obj_cast.read_unaligned();
                                        let _ = (*header).free_list_head.compare_exchange(
                                            u16::try_from(i).unwrap(),
                                            next.unwrap_or(u16::MAX),
                                            Ordering::AcqRel,
                                            Ordering::Acquire,
                                        );
                                    }
                                    current_free = if (*header).free_list_head()
                                        == Some(u16::try_from(i).unwrap())
                                    {
                                        None
                                    } else {
                                        (*header).free_list_head()
                                    };
                                    obj_cast.write_unaligned(current_free);
                                } else {
                                    current_free = next_head;
                                }
                                continue;
                            }
                            break;
                        }
                        Err(actual) => {
                            current_free = if actual == u16::MAX {
                                None
                            } else {
                                Some(actual)
                            };
                            obj_cast.write_unaligned(current_free);
                        }
                    }
                }

                (*header).clear_allocated(i);
                reclaimed += 1;
            }
            (*header).clear_mark(i);
        }
    }

    reclaimed
}

#[cfg(feature = "lazy-sweep")]
#[must_use]
/// Sweep up to `num_pages` pages that need lazy sweeping.
///
/// Returns the number of pages actually swept.
///
/// # Safety
///
/// - `heap` must be a valid `LocalHeap` owned by the current thread
/// - Only pages owned by this heap are swept (`PAGE_FLAG_LARGE` pages are skipped)
/// - The heap's pages vector is not modified, only page metadata and free lists
/// - Safe to call during allocation when pages need sweeping
pub fn sweep_pending(heap: &mut LocalHeap, num_pages: usize) -> usize {
    let mut swept = 0;
    let mut pages_to_sweep: Vec<NonNull<PageHeader>> = heap
        .pages
        .iter()
        .filter(|&&page_ptr| unsafe {
            let header = page_ptr.as_ptr();
            let hdr = header.read();
            !hdr.is_large_object() && hdr.needs_sweep()
        })
        .copied()
        .take(num_pages)
        .collect();

    for page_ptr in pages_to_sweep {
        unsafe {
            let header = page_ptr.as_ptr();
            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);

            if header.read().all_dead() {
                lazy_sweep_page_all_dead(page_ptr, block_size, obj_count, header_size);
                (*header).clear_all_dead();
                std::sync::atomic::fence(Ordering::Release);
                (*header).clear_needs_sweep();
                (*header).set_dead_count(0);
                swept += 1;
            } else {
                let (reclaimed, all_dead) =
                    lazy_sweep_page(page_ptr, block_size, obj_count, header_size);
                if reclaimed > 0 {
                    if all_dead && reclaimed == obj_count {
                        (*header).set_all_dead();
                    }
                    if reclaimed == obj_count {
                        std::sync::atomic::fence(Ordering::Release);
                        (*header).clear_needs_sweep();
                        (*header).set_dead_count(0);
                        (*header).clear_all_dead();
                    } else {
                        debug_assert!(
                            reclaimed <= (*header).dead_count() as usize,
                            "reclaimed {} objects but dead_count is {}",
                            reclaimed,
                            (*header).dead_count()
                        );
                        #[allow(clippy::cast_possible_truncation)]
                        (*header).set_dead_count((*header).dead_count() - reclaimed as u16);
                    }
                    swept += 1;
                } else if (*header).is_fully_marked() {
                    std::sync::atomic::fence(Ordering::Release);
                    (*header).clear_needs_sweep();
                    (*header).set_dead_count(0);
                }
            }
        }
    }

    swept
}

#[cfg(feature = "lazy-sweep")]
#[allow(unsafe_op_in_unsafe_fn)]
/// Sweep a specific page, returning the number of reclaimed objects.
///
/// Unlike `sweep_pending` which sweeps arbitrary pages from the heap,
/// this function sweeps the exact page specified. This is used by
/// `alloc_from_pending_sweep` to ensure pages with dead objects are
/// swept before attempting allocation.
///
/// # Safety
///
/// - `heap` must be a valid `LocalHeap` owned by the current thread
/// - `page_ptr` must point to a valid `PageHeader` for a page owned by this heap
/// - Only non-large pages are swept (large objects use eager sweep)
/// - The heap's pages vector is not modified, only page metadata and free lists
/// - Safe to call during allocation when pages need sweeping
pub unsafe fn sweep_specific_page(
    heap: &mut LocalHeap,
    page_ptr: NonNull<crate::heap::PageHeader>,
    _num_pages: usize,
) -> usize {
    let mut reclaimed = 0;
    unsafe {
        let header = page_ptr.as_ptr();
        let block_size = (*header).block_size as usize;
        let obj_count = (*header).obj_count as usize;
        let header_size = crate::heap::PageHeader::header_size(block_size);

        if header.read().all_dead() {
            lazy_sweep_page_all_dead(page_ptr, block_size, obj_count, header_size);
            (*header).clear_all_dead();
            std::sync::atomic::fence(Ordering::Release);
            (*header).clear_needs_sweep();
            (*header).set_dead_count(0);
            reclaimed = obj_count;
        } else {
            let (reclaimed_count, all_dead) =
                lazy_sweep_page(page_ptr, block_size, obj_count, header_size);
            if reclaimed_count > 0 {
                if all_dead && reclaimed_count == obj_count {
                    (*header).set_all_dead();
                }
                if reclaimed_count == obj_count {
                    std::sync::atomic::fence(Ordering::Release);
                    (*header).clear_needs_sweep();
                    (*header).set_dead_count(0);
                    (*header).clear_all_dead();
                } else {
                    debug_assert!(
                        reclaimed_count <= (*header).dead_count() as usize,
                        "reclaimed {} objects but dead_count is {}",
                        reclaimed_count,
                        (*header).dead_count()
                    );
                    #[allow(clippy::cast_possible_truncation)]
                    (*header).set_dead_count((*header).dead_count() - reclaimed_count as u16);
                }
                reclaimed = reclaimed_count;
            } else if (*header).is_fully_marked() {
                std::sync::atomic::fence(Ordering::Release);
                (*header).clear_needs_sweep();
                (*header).set_dead_count(0);
            }
        }
    }

    reclaimed
}

#[cfg(feature = "lazy-sweep")]
#[must_use]
/// Returns the number of pages currently awaiting lazy sweep.
///
/// # Safety
///
/// - `heap` must be a valid `LocalHeap` owned by the current thread
/// - This function only reads page metadata, no modifications are made
/// - Safe to call from any context
pub fn pending_sweep_count(heap: &LocalHeap) -> usize {
    heap.pages
        .iter()
        .filter(|&&page_ptr| unsafe {
            let header = page_ptr.as_ptr();
            let hdr = header.read();
            !hdr.is_large_object() && hdr.needs_sweep()
        })
        .count()
}

// ============================================================================
// GcVisitor - Unified Visitor implementation
// ============================================================================

impl GcVisitor {
    #[inline]
    pub fn new(kind: VisitorKind) -> Self {
        Self {
            kind,
            worklist: Vec::with_capacity(1024),
            objects_marked: 0,
        }
    }

    /// Get the count of objects marked by this visitor.
    #[inline]
    pub const fn objects_marked(&self) -> usize {
        self.objects_marked
    }

    #[inline]
    pub fn process_worklist(&mut self) {
        while let Some(ptr) = self.worklist.pop() {
            unsafe {
                let ptr_addr = ptr.as_ptr() as *const u8;
                let header = crate::heap::ptr_to_page_header(ptr_addr);

                if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
                    continue;
                }

                if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
                    (*header.as_ptr()).set_mark(idx);
                } else {
                    continue;
                }

                ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), self);
            }
        }
    }

    #[allow(dead_code)]
    unsafe fn visit_region(&mut self, ptr: *const u8, len: usize) {
        unsafe {
            crate::scan::scan_heap_region_conservatively(ptr, len, self);
        }
    }
}

impl Visitor for GcVisitor {
    #[inline]
    fn visit<T: Trace>(&mut self, gc: &crate::Gc<T>) {
        let raw = gc.raw_ptr();
        if !raw.is_null() {
            let ptr = raw.cast::<crate::ptr::GcBox<()>>();

            unsafe {
                let ptr_addr = ptr as *const u8;
                let header = crate::heap::ptr_to_page_header(ptr_addr);

                if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
                    return;
                }

                if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
                    if self.kind == VisitorKind::Minor && (*header.as_ptr()).generation > 0 {
                        return;
                    }

                    if (*header.as_ptr()).is_marked(idx) {
                        return;
                    }
                    (*header.as_ptr()).set_mark(idx);
                    self.objects_marked += 1;
                } else {
                    return;
                }

                self.worklist.push(std::ptr::NonNull::new_unchecked(ptr));
            }
        }
    }

    unsafe fn visit_region(&mut self, ptr: *const u8, len: usize) {
        unsafe {
            crate::scan::scan_heap_region_conservatively(ptr, len, self);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_info() {
        let info = CollectInfo {
            n_gcs_dropped: 5,
            n_gcs_existing: 10,
            heap_size: 1024,
            young_size: 512,
            old_size: 512,
        };

        assert_eq!(info.n_gcs_dropped_since_last_collect(), 5);
        assert_eq!(info.n_gcs_existing(), 10);
        assert_eq!(info.heap_size(), 1024);
        assert_eq!(info.young_size(), 512);
        assert_eq!(info.old_size(), 512);
    }

    #[test]
    fn test_default_collect_condition() {
        // Should not collect when drops < existing
        let info = CollectInfo {
            n_gcs_dropped: 5,
            n_gcs_existing: 10,
            heap_size: 1024,
            young_size: 0,
            old_size: 1024,
        };
        assert!(!default_collect_condition(&info));

        // Should collect when drops > existing
        let info = CollectInfo {
            n_gcs_dropped: 15,
            n_gcs_existing: 10,
            heap_size: 1024,
            young_size: 0,
            old_size: 1024,
        };
        assert!(default_collect_condition(&info));
    }
    #[test]
    fn test_minor_collection() {
        // 1. Allocate objects in Young Gen
        crate::heap::with_heap(|_| {}); // Ensure heap initialized
        clear_test_roots();

        // ROOTING: Use a stack-allocated array to ensure pointers are visible
        // to the conservative stack scanner. A Vec's buffer is on the heap
        // and would be invisible.
        let keep: [crate::Gc<i32>; 20] = [
            crate::Gc::new(0),
            crate::Gc::new(1),
            crate::Gc::new(2),
            crate::Gc::new(3),
            crate::Gc::new(4),
            crate::Gc::new(5),
            crate::Gc::new(6),
            crate::Gc::new(7),
            crate::Gc::new(8),
            crate::Gc::new(9),
            crate::Gc::new(10),
            crate::Gc::new(11),
            crate::Gc::new(12),
            crate::Gc::new(13),
            crate::Gc::new(14),
            crate::Gc::new(15),
            crate::Gc::new(16),
            crate::Gc::new(17),
            crate::Gc::new(18),
            crate::Gc::new(19),
        ];

        for g in &keep {
            register_test_root(crate::ptr::Gc::internal_ptr(g));
        }

        let drop_me = crate::Gc::new(123);
        let _drop_addr = crate::Gc::as_ptr(&drop_me);
        drop(drop_me);

        // 2. Trigger Minor GC explicitly
        crate::heap::with_heap(|heap| {
            collect_minor(heap);

            // 3. Verify survivors promoted
            // Check if 'keep' objects are now in Old Gen (Gen 1)
            let ptr = crate::Gc::as_ptr(&keep[0]);
            unsafe {
                let page = crate::heap::ptr_to_page_header(ptr.cast());
                assert_eq!(
                    (*page.as_ptr()).generation,
                    1,
                    "Survivors should be promoted to Old Gen"
                );
            }

            // 4. Verify dropped object collected/swept?
            assert!(
                heap.young_allocated() == 0,
                "Young gen should be empty (promoted or swept)"
            );
            assert!(heap.old_allocated() > 0, "Old gen should contain survivors");
        });
        clear_test_roots();
    }

    #[test]
    fn test_write_barrier() {
        use crate::cell::GcCell;

        // 1. Create Old Gen object
        clear_test_roots();
        let old_cell = crate::Gc::new(GcCell::new(None));
        register_test_root(crate::ptr::Gc::internal_ptr(&old_cell));

        // Force promotion
        crate::heap::with_heap(collect_minor);

        {
            let ptr = crate::Gc::as_ptr(&old_cell);
            unsafe {
                let page = crate::heap::ptr_to_page_header(ptr.cast());
                assert_eq!((*page.as_ptr()).generation, 1);
            }
        }

        // 2. Create Young Gen object
        let young = crate::Gc::new(100);

        // 3. Trigger Write Barrier
        // old -> young
        *old_cell.borrow_mut() = Some(young.clone());

        // Verify dirty bit set
        {
            let ptr = crate::Gc::as_ptr(&old_cell);
            unsafe {
                let page = crate::heap::ptr_to_page_header(ptr.cast());
                let idx = crate::heap::ptr_to_object_index(ptr.cast()).unwrap();
                assert!(
                    (*page.as_ptr()).is_dirty(idx),
                    "Write barrier should set dirty bit"
                );
            }
        }

        // 4. Drop strong ref to young, keep only via old
        drop(young);

        // 5. Collect Minor
        crate::heap::with_heap(collect_minor);

        // 6. Verify Young object survived (accessible via old_cell)
        assert_eq!(**old_cell.borrow().as_ref().unwrap(), 100);
        clear_test_roots();
    }

    #[test]
    fn test_metrics() {
        let x = crate::Gc::new(42i32);
        crate::collect_full();

        // Check metrics immediately after collect_full, before drop(x)
        // which might trigger another (Minor) collection
        let metrics = crate::last_gc_metrics();

        let _ = metrics.total_collections;
        let _ = metrics.collection_type;
        let _ = metrics.duration;
        let _ = metrics.bytes_reclaimed;
        let _ = metrics.bytes_surviving;
        let _ = metrics.objects_reclaimed;
        let _ = metrics.objects_surviving;

        assert!(metrics.total_collections > 0, "No collections recorded!");
        assert_eq!(
            metrics.collection_type,
            crate::metrics::CollectionType::Major
        );

        drop(x);
    }

    #[test]
    fn test_multi_threaded_gc_handshake() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        clear_test_roots();

        let num_threads = 4;
        let objects_per_thread = 10; // Reduced to speed up test

        let barrier = Arc::new(Barrier::new(num_threads));
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut handles = Vec::new();
        for i in 0..num_threads {
            let barrier = barrier.clone();
            let started = started.clone();
            let done = done.clone();

            let handle = thread::spawn(move || {
                barrier.wait();
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                for j in 0..objects_per_thread {
                    let val = i * 1000 + j;
                    let _gc_val = crate::Gc::new(val);

                    // Call safepoint - this may trigger GC
                    crate::safepoint();
                }

                done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            started.load(std::sync::atomic::Ordering::SeqCst),
            num_threads
        );
        assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), num_threads);

        clear_test_roots();
    }

    #[test]
    fn test_gc_requested_flag() {
        use std::sync::atomic::Ordering;

        assert!(
            !crate::heap::GC_REQUESTED.load(Ordering::Relaxed),
            "GC_REQUESTED should be false initially"
        );

        let _guard = crate::heap::thread_registry().lock().unwrap();

        crate::heap::GC_REQUESTED.store(true, Ordering::Relaxed);
        assert!(
            crate::heap::GC_REQUESTED.load(Ordering::Relaxed),
            "GC_REQUESTED should be true after setting"
        );

        crate::heap::GC_REQUESTED.store(false, Ordering::Relaxed);
        assert!(
            !crate::heap::GC_REQUESTED.load(Ordering::Relaxed),
            "GC_REQUESTED should be false after clearing"
        );
    }

    #[test]
    fn test_thread_control_block_state() {
        use std::sync::atomic::Ordering;

        crate::heap::with_heap_and_tcb(|_, tcb| {
            assert_eq!(
                tcb.state.load(Ordering::Relaxed),
                crate::heap::THREAD_STATE_EXECUTING,
                "Thread should be in EXECUTING state initially"
            );

            tcb.state
                .store(crate::heap::THREAD_STATE_SAFEPOINT, Ordering::Relaxed);
            assert_eq!(
                tcb.state.load(Ordering::Relaxed),
                crate::heap::THREAD_STATE_SAFEPOINT,
                "Thread state should be SAFEPOINT after setting"
            );

            tcb.state
                .store(crate::heap::THREAD_STATE_INACTIVE, Ordering::Relaxed);
            assert_eq!(
                tcb.state.load(Ordering::Relaxed),
                crate::heap::THREAD_STATE_INACTIVE,
                "Thread state should be INACTIVE after setting"
            );
        });
    }

    #[test]
    fn test_safepoint_function() {
        crate::safepoint();

        for _ in 0..100 {
            crate::Gc::new(42i32);
        }

        crate::safepoint();
    }

    #[test]
    fn test_drop_accesses_other_gc_object() {
        use std::cell::Cell;

        thread_local! {
            static DROP_COUNT: Cell<usize> = const { Cell::new(0) };
        }

        struct DropChecker {
            other: Option<crate::Gc<i32>>,
        }

        unsafe impl crate::Trace for DropChecker {
            fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
                if let Some(ref other) = self.other {
                    visitor.visit(other);
                }
            }
        }

        impl Drop for DropChecker {
            fn drop(&mut self) {
                if let Some(ref other) = self.other {
                    let _ = **other;
                }
                DROP_COUNT.with(|c| c.set(c.get() + 1));
            }
        }

        {
            let a = crate::Gc::new(42);
            let checker = crate::Gc::new(DropChecker {
                other: Some(a.clone()),
            });
            drop(a);
            drop(checker);
        }

        crate::collect_full();

        assert!(DROP_COUNT.with(std::cell::Cell::get) >= 1);
    }
}
