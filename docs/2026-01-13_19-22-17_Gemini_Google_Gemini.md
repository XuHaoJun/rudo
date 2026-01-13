---
title: "Google Gemini"
platform: Gemini
url: https://gemini.google.com/u/1/app/975d1a744cda14fe?hl=zh-TW
exported: 2026-01-13T11:22:17.625Z
---

# Google Gemini

## 使用者

```gc.rs

//! Mark-Sweep garbage collection algorithm.

//!

//! This module implements the core garbage collection logic using

//! a mark-sweep algorithm with the `BiBOP` memory layout.

use std::cell::Cell;

use std::collections::HashMap;

use std::ptr::NonNull;

use std::sync::atomic::Ordering;

use crate::heap::{LocalHeap, PageHeader};

use crate::ptr::GcBox;

use crate::trace::{GcVisitor, Trace, Visitor, VisitorKind};

/// Information about an object pending deallocation.

/// Used in two-phase sweep: phase 1 drops, phase 2 reclaims.

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

    #[cfg(any(test, feature = "test-util"))]

    static TEST_ROOTS: std::cell::RefCell<Vec<*const u8>> = const { std::cell::RefCell::new(Vec::new()) };

}

/// Register a root for GC marking. This is useful for tests where Miri cannot find

/// roots via conservative stack scanning.

#[cfg(any(test, feature = "test-util"))]

pub fn register_test_root(ptr: *const u8) {

    TEST_ROOTS.with(|roots| roots.borrow_mut().push(ptr));

}

/// Clear all registered test roots.

#[cfg(any(test, feature = "test-util"))]

pub fn clear_test_roots() {

    TEST_ROOTS.with(|roots| roots.borrow_mut().clear());

}

/// Notify that a Gc was created.

pub fn notify_created_gc() {

    N_EXISTING.with(|n| n.set(n.get() + 1));

}

pub fn notify_dropped_gc() {

    N_DROPS.with(|n| n.set(n.get() + 1));

    maybe_collect();

}

fn maybe_collect() {

    if IN_COLLECT.with(Cell::get) {

        return;

    }

    // Check if we should collect

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

        return; // Already borrowed, skip collection check

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

fn perform_multi_threaded_collect() {

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

        for tcb in &tcbs {

            unsafe {

                clear_all_marks_and_dirty(&*tcb.heap.get());

            }

        }

        // Phase 2: Mark all reachable objects (tracing across all heaps)

        // We mark from each heap's perspective to ensure we find all cross-heap references

        for tcb in &tcbs {

            unsafe {

                mark_major_roots_multi(&mut *tcb.heap.get(), &all_stack_roots);

            }

        }

        // Phase 3: Sweep ALL heaps

        for tcb in &tcbs {

            unsafe {

                let reclaimed = sweep_segment_pages(&*tcb.heap.get(), false);

                let reclaimed_large = sweep_large_objects(&mut *tcb.heap.get(), false);

                objects_reclaimed += reclaimed + reclaimed_large;

                promote_all_pages(&*tcb.heap.get());

            }

        }

        crate::heap::sweep_orphan_pages();

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

/// Wake up any threads waiting at a safe point.

/// This is used when a non-collector thread needs to wake up waiting threads

/// and perform single-threaded collection. It properly restores threads to

/// EXECUTING state and restores `active_count`.

fn wake_waiting_threads() {

    let registry = crate::heap::thread_registry().lock().unwrap();

    let mut woken_count = 0;

    for tcb in &registry.threads {

        if tcb.state.load(Ordering::Acquire) == crate::heap::THREAD_STATE_SAFEPOINT {

            tcb.gc_requested.store(false, Ordering::Relaxed);

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

    // Atomically clear GC flag and wake threads while holding registry lock

    // This prevents race condition where threads see stale GC_REQUESTED value

    {

        let registry = crate::heap::thread_registry().lock().unwrap();

        // Clear global flag first with SeqCst ordering

        crate::heap::GC_REQUESTED.store(false, Ordering::SeqCst);

        // Wake any threads already at safepoints and clear their flags

        let mut woken_count = 0;

        for tcb in &registry.threads {

            if tcb.state.load(Ordering::Acquire) == crate::heap::THREAD_STATE_SAFEPOINT {

                tcb.gc_requested.store(false, Ordering::SeqCst);

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

    IN_COLLECT.with(|in_collect| in_collect.set(true));

    let start = std::time::Instant::now();

    let before_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    let mut objects_reclaimed = 0;

    crate::heap::with_heap(|heap| {

        objects_reclaimed = collect_major(heap);

    });

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

fn perform_multi_threaded_collect_full() {

    IN_COLLECT.with(|in_collect| in_collect.set(true));

    let start = std::time::Instant::now();

    let before_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    let mut objects_reclaimed = 0;

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

    for tcb in &tcbs {

        unsafe {

            objects_reclaimed += collect_major_multi(&mut *tcb.heap.get(), &all_stack_roots);

        }

    }

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

fn collect_major_multi(

    heap: &mut LocalHeap,

    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],

) -> usize {

    clear_all_marks_and_dirty(heap);

    mark_major_roots_multi(heap, stack_roots);

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

    let mut visitor = GcVisitor {

        kind: VisitorKind::Minor,

    };

    // Scan all threads' captured stack roots (passed in, not consumed)

    for &(ptr, _) in stack_roots {

        unsafe {

            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {

                mark_object_minor(gc_box, &mut visitor);

            }

        }

    }

    // Also scan current thread's registers (collector thread's live registers)

    unsafe {

        crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {

            if let Some(gc_box_ptr) =

                crate::heap::find_gc_box_from_ptr(heap, potential_ptr as *const u8)

            {

                mark_object_minor(gc_box_ptr, &mut visitor);

            }

        });

    }

    #[cfg(any(test, feature = "test-util"))]

    TEST_ROOTS.with(|roots| {

        for &ptr in roots.borrow().iter() {

            unsafe {

                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {

                    mark_object_minor(gc_box, &mut visitor);

                }

            }

        }

    });

    for page_ptr in heap.all_pages() {

        unsafe {

            let header = page_ptr.as_ptr();

            if (*header).generation == 0 {

                continue;

            }

            if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {

                continue;

            }

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

            (*header).clear_all_dirty();

        }

    }

}

/// Mark roots from all threads' stacks for Major GC.

fn mark_major_roots_multi(

    heap: &mut LocalHeap,

    stack_roots: &[(*const u8, std::sync::Arc<crate::heap::ThreadControlBlock>)],

) {

    let mut visitor = GcVisitor {

        kind: VisitorKind::Major,

    };

    // Scan all threads' captured stack roots (passed in, not consumed)

    for &(ptr, _) in stack_roots {

        unsafe {

            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {

                mark_object(gc_box, &mut visitor);

            }

        }

    }

    // Also scan current thread's registers (collector thread's live registers)

    unsafe {

        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {

            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {

                mark_object(gc_box, &mut visitor);

            }

        });

    }

    #[cfg(any(test, feature = "test-util"))]

    TEST_ROOTS.with(|roots| {

        for &ptr in roots.borrow().iter() {

            unsafe {

                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {

                    mark_object(gc_box, &mut visitor);

                }

            }

        }

    });

}

/// Minor Collection: Collect Young Generation only.

fn collect_minor(heap: &mut LocalHeap) -> usize {

    // 1. Mark Phase

    mark_minor_roots(heap);

    // 2. Sweep Phase

    let reclaimed = sweep_segment_pages(heap, true);

    let reclaimed_large = sweep_large_objects(heap, true);

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

                for i in 0..4 {

                    let bits = (*header).allocated_bitmap[i];

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

fn collect_major(heap: &mut LocalHeap) -> usize {

    // 1. Mark Phase

    // Clear marks first

    clear_all_marks_and_dirty(heap);

    mark_major_roots(heap);

    // 2. Sweep Phase

    let reclaimed = sweep_segment_pages(heap, false);

    let reclaimed_large = sweep_large_objects(heap, false);

    // 3. Promotion Phase (All to Old)

    promote_all_pages(heap);

    reclaimed + reclaimed_large

}

/// Clear all mark bits and dirty bits in the heap.

fn clear_all_marks_and_dirty(heap: &LocalHeap) {

    for page_ptr in heap.all_pages() {

        // SAFETY: Page pointers in the heap are always valid

        unsafe {

            let header = page_ptr.as_ptr();

            (*header).clear_all_marks();

            (*header).clear_all_dirty();

        }

    }

}

/// Mark roots for Minor GC (Stack + `RemSet`).

fn mark_minor_roots(heap: &LocalHeap) {

    let mut visitor = GcVisitor {

        kind: VisitorKind::Minor,

    };

    // 1. Scan Stack

    unsafe {

        crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {

            if let Some(gc_box_ptr) =

                crate::heap::find_gc_box_from_ptr(heap, potential_ptr as *const u8)

            {

                // Only mark if it points to Young object.

                // But mark_object_minor handles the check.

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

    // 2. Scan Dirty Old Objects (RemSet)

    for page_ptr in heap.all_pages() {

        unsafe {

            let header = page_ptr.as_ptr();

            // Skip Young Pages (they are scanned via stack/tracing)

            if (*header).generation == 0 {

                continue;

            }

            // Skip Large Objects (assumed not dirty inner pointers for now)

            if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {

                continue;

            }

            // Iterate Dirty Bitmap

            let obj_count = (*header).obj_count as usize;

            for i in 0..obj_count {

                if (*header).is_dirty(i) {

                    // Found dirty old object. Trace it!

                    let block_size = (*header).block_size as usize;

                    let header_size = PageHeader::header_size(block_size);

                    let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));

                    #[allow(clippy::cast_ptr_alignment)]

                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                    // Use the trace_fn to find pointers to Young Gen

                    // SAFETY: We are already in an unsafe block.

                    ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);

                }

            }

            // Clear dirty bits for this page after processing

            (*header).clear_all_dirty();

        }

    }

}

/// Mark roots for Major GC (Stack).

fn mark_major_roots(heap: &LocalHeap) {

    let mut visitor = GcVisitor {

        kind: VisitorKind::Major,

    };

    unsafe {

        // 1. Mark stack roots (Conservative)

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

}

/// Mark object for Minor GC.

unsafe fn mark_object_minor(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {

    let ptr_addr = ptr.as_ptr() as *const u8;

    let page_addr = (ptr_addr as usize) & crate::heap::PAGE_MASK;

    // SAFETY: ptr_addr is a valid pointer to a GcBox

    let header = unsafe { crate::heap::ptr_to_page_header(ptr_addr) };

    // SAFETY: We're inside an unsafe fn, but unsafe_op_in_unsafe_fn requires block

    unsafe {

        if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {

            return;

        }

        // IF OLD GENERATION: STOP.

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

        // Trace children using value's trace_fn

        ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), visitor);

    }

}

/// Sweep pages in regular segments.

///

/// Two-phase sweep to prevent Use-After-Free during Drop:

/// - Phase 1: Execute all Drop functions (objects still accessible)

/// - Phase 2: Reclaim memory and rebuild free lists

fn sweep_segment_pages(heap: &LocalHeap, only_young: bool) -> usize {

    let pending = sweep_phase1_finalize(heap, only_young);

    sweep_phase2_reclaim(heap, pending, only_young)

}

/// Phase 1: Execute Drop functions for all dead objects.

///

/// This phase only calls `drop_fn` but does NOT reclaim memory yet.

/// This ensures that during Drop, all other GC objects are still accessible.

fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {

    let mut pending = Vec::new();

    for page_ptr in heap.all_pages() {

        unsafe {

            let header = page_ptr.as_ptr();

            if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {

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

fn sweep_phase2_reclaim(heap: &LocalHeap, pending: Vec<PendingDrop>, only_young: bool) -> usize {

    let mut reclaimed = 0;

    // Group pending drops by page for efficient processing

    let mut pending_by_page: HashMap<usize, Vec<usize>> = HashMap::new();

    for p in pending {

        pending_by_page

            .entry(p.page.as_ptr() as usize)

            .or_default()

            .push(p.index);

    }

    // Process each page: rebuild free list from scratch

    for page_ptr in heap.all_pages() {

        unsafe {

            let header = page_ptr.as_ptr();

            if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {

                continue;

            }

            if only_young && (*header).generation > 0 {

                continue;

            }

            let block_size = (*header).block_size as usize;

            let obj_count = (*header).obj_count as usize;

            let header_size = PageHeader::header_size(block_size);

            let page_addr = header.cast::<u8>();

            // Check if this page has any pending reclaims

            let pending_indices = pending_by_page.get(&(header as usize));

            // Clear allocated bits for pending slots

            if let Some(indices) = pending_indices {

                for &index in indices {

                    (*header).clear_allocated(index);

                    reclaimed += 1;

                }

            }

            // Rebuild free list from scratch (iterate in reverse for correct allocation order)

            let mut free_head: Option<u16> = None;

            for i in (0..obj_count).rev() {

                if !(*header).is_allocated(i) {

                    // Slot is free - add to free list

                    let obj_ptr = page_addr.add(header_size + i * block_size);

                    #[allow(clippy::cast_ptr_alignment)]

                    let obj_cast = obj_ptr.cast::<Option<u16>>();

                    *obj_cast = free_head;

                    #[allow(clippy::cast_possible_truncation)]

                    {

                        free_head = Some(i as u16);

                    }

                }

            }

            (*header).free_list_head = free_head;

        }

    }

    N_EXISTING.with(|n| n.set(n.get().saturating_sub(reclaimed)));

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

/// Mark a single object and trace its children.

///

/// # Safety

///

/// The pointer must be a valid `GcBox` pointer.

unsafe fn mark_object(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {

    // Get the page header

    let ptr_addr = ptr.as_ptr() as *const u8;

    // SAFETY: ptr is a valid GcBox pointer

    let header = unsafe { crate::heap::ptr_to_page_header(ptr_addr) };

    unsafe {

        // Validate this is a GC page

        if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {

            return;

        }

        // Use 1-arg ptr_to_object_index which calls ptr_to_page_header internally

        // Note: ptr_to_object_index checks for MAGIC_GC_PAGE and bounds.

        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {

            if (*header.as_ptr()).is_marked(idx) {

                return; // Already marked

            }

            // Mark this object

            (*header.as_ptr()).set_mark(idx);

        } else {

            return; // Invalid object index

        }

        // Trace children using value's trace_fn

        ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), visitor);

    }

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

                    let pages_needed = total_size.div_ceil(crate::heap::PAGE_SIZE);

                    let alloc_size = pages_needed * crate::heap::PAGE_SIZE;

                    ((*gc_box_ptr).drop_fn)(obj_ptr);

                    to_deallocate.push((page_ptr, alloc_size, pages_needed));

                }

            }

        }

    }

    let mut reclaimed = 0;

    for (page_ptr, alloc_size, pages_needed) in to_deallocate {

        unsafe {

            let header_addr = page_ptr.as_ptr() as usize;

            heap.pages.retain(|&p| p != page_ptr);

            for p in 0..pages_needed {

                let page_addr = header_addr + (p * crate::heap::PAGE_SIZE);

                heap.large_object_map.remove(&page_addr);

            }

            {

                let mut manager = crate::heap::segment_manager()

                    .lock()

                    .expect("segment manager lock poisoned");

                for p in 0..pages_needed {

                    let page_addr = header_addr + (p * crate::heap::PAGE_SIZE);

                    manager.large_object_map.remove(&page_addr);

                }

            }

            sys_alloc::Mmap::from_raw(page_ptr.as_ptr().cast::<u8>(), alloc_size);

            reclaimed += 1;

            N_EXISTING.with(|n| n.set(n.get().saturating_sub(1)));

        }

    }

    reclaimed

}

// ============================================================================

// GcVisitor - Unified Visitor implementation

// ============================================================================

impl Visitor for GcVisitor {

    fn visit<T: Trace + ?Sized>(&mut self, gc: &crate::Gc<T>) {

        if let Some(ptr) = gc.raw_ptr().as_option() {

            unsafe {

                if self.kind == VisitorKind::Minor {

                    mark_object_minor(ptr.cast(), self);

                } else {

                    mark_object(ptr.cast(), self);

                }

            }

        }

    }

}

```

```heap.rs

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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use std::sync::{Condvar, Mutex, OnceLock};

use sys_alloc::{Mmap, MmapOptions};

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

/// Flag: Page is a large object.

pub const PAGE_FLAG_LARGE: u8 = 0x01;

/// Flag: Page is an orphan (owner thread has terminated).

pub const PAGE_FLAG_ORPHAN: u8 = 0x02;

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

    pub flags: u8,

    /// Padding for alignment.

    _padding: [u8; 2],

    /// Bitmap of marked objects (atomic for concurrent marking).

    pub mark_bitmap: [AtomicU64; 4],

    /// Bitmap of dirty objects (atomic for concurrent write barriers).

    pub dirty_bitmap: [AtomicU64; 4],

    /// Bitmap of allocated objects (non-atomic, only modified by owner thread).

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

            word.store(0, Ordering::Release);

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

        self.dirty_bitmap[word].fetch_or(1u64 << bit, Ordering::Release);

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

            word.store(0, Ordering::Release);

        }

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

    /// Orphan pages: pages from terminated threads that may contain live objects.

    pub orphan_pages: Vec<OrphanPage>,

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

            orphan_pages: Vec::new(),

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

                if ((*header).flags & PAGE_FLAG_LARGE) == 0

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

                _padding: [0; 2],

                mark_bitmap: [

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                ],

                dirty_bitmap: [

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                ],

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

                flags: PAGE_FLAG_LARGE,

                _padding: [0; 2],

                mark_bitmap: [

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                ],

                dirty_bitmap: [

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                    AtomicU64::new(0),

                ],

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

            .filter(|p| unsafe { (p.as_ptr().read().flags & PAGE_FLAG_LARGE) != 0 })

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

            .filter(|p| unsafe { (p.as_ptr().read().flags & PAGE_FLAG_LARGE) != 0 })

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

        let current_thread = std::thread::current().id();

        let mut manager = segment_manager()

            .lock()

            .expect("segment manager lock poisoned");

        for page_ptr in std::mem::take(&mut self.pages) {

            unsafe {

                let header = page_ptr.as_ptr();

                if (*header).magic != MAGIC_GC_PAGE {

                    continue;

                }

                let is_large = ((*header).flags & PAGE_FLAG_LARGE) != 0;

                let block_size = (*header).block_size as usize;

                let header_size = (*header).header_size as usize;

                let size = if is_large {

                    let total = header_size + block_size;

                    total.div_ceil(PAGE_SIZE) * PAGE_SIZE

                } else {

                    PAGE_SIZE

                };

                (*header).flags |= PAGE_FLAG_ORPHAN;

                manager.orphan_pages.push(OrphanPage {

                    addr: page_ptr.as_ptr() as usize,

                    size,

                    is_large,

                    original_owner: current_thread,

                });

                if is_large {

                    let header_addr = header as usize;

                    for p in 0..(size / PAGE_SIZE) {

                        let page_addr = header_addr + (p * PAGE_SIZE);

                        manager.large_object_map.remove(&page_addr);

                    }

                }

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

    let mut manager = segment_manager().lock().unwrap();

    let mut to_reclaim = Vec::new();

    manager.orphan_pages.retain(|orphan| unsafe {

        let header = orphan.addr as *mut PageHeader;

        let has_survivors = if orphan.is_large {

            (*header).is_marked(0)

        } else {

            let obj_count = (*header).obj_count as usize;

            (0..obj_count).any(|i| (*header).is_marked(i))

        };

        if has_survivors {

            (*header).clear_all_marks();

            true

        } else {

            to_reclaim.push((orphan.addr, orphan.size));

            false

        }

    });

    drop(manager);

    for (addr, size) in to_reclaim {

        unsafe {

            let header = addr as *mut PageHeader;

            let is_large = ((*header).flags & PAGE_FLAG_LARGE) != 0;

            if is_large {

                let header_size = (*header).header_size as usize;

                let obj_ptr = (addr as *mut u8).add(header_size);

                #[allow(clippy::cast_ptr_alignment)]

                let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();

                if !(*gc_box_ptr).is_value_dead() {

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

                        if !(*gc_box_ptr).is_value_dead() {

                            ((*gc_box_ptr).drop_fn)(obj_ptr);

                        }

                    }

                }

            }

            sys_alloc::Mmap::from_raw(addr as *mut u8, size);

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

        if header.flags & PAGE_FLAG_LARGE != 0 {

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

```

你是 R. Kent Dybvig, 請協助我 debug cargo test 有機率 hang on 問題, 但是設定 threads=1 卻沒有這個問題, 這件事是新增 Two-Phase Sweep 後發生的，有需要觀察更多 crate rust code 請告知我，個人懷疑是 GlobalSegmentManager race 的問題。

## Gemini

這是一個非常典型且棘手的 Garbage Collection (GC) 實作問題。我是 R. Kent Dybvig，我們來仔細檢視你的 Mark-Sweep 實作。

你遇到的 `cargo test` hang on (卡死) 問題，且僅在 `threads > 1` 時發生，通常直覺會指向 Deadlock 或 Race Condition。你懷疑 `GlobalSegmentManager` 是合理的，但在檢視你的 Two-Phase Sweep 實作後，我發現了一個更隱蔽、且極易導致 "Undefined Behavior" (UB) 的問題。這個 UB 可能表現為 Segfault，也可能表現為 Infinite Loop (看起來像 Hang)。

這個問題的核心在於 **"Reentrancy" (重入) 與 "Iterator Invalidation" (迭代器失效)**。

### 問題分析：在 Sweep 期間的 Allocation

請看 `gc.rs` 中的 `sweep_phase1_finalize` 函數：

```rust
// gc.rs

fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {
    let mut pending = Vec::new();

    // [CRITICAL ERROR]
    // 你正在迭代 `heap.all_pages()` (實際上是 heap.pages 的 iterator)
    for page_ptr in heap.all_pages() {
        unsafe {
            // ... (省略)

            // 這裡呼叫了 User 定義的 drop_fn
            // 如果這個 drop_fn 裡面做了 allocation (產生新物件) 怎麼辦？
            ((*gc_box_ptr).drop_fn)(obj_ptr);

            // ...
        }
    }
    pending
}
```

**發生了什麼事？**

1. Phase 1 (Finalize) : 你正在遍歷 heap.pages ( Vec<NonNull<PageHeader>> )。
2. drop_fn 執行 : 使用者的 destructor 被呼叫。
3. Allocation : 假設使用者的 destructor 需要配置記憶體 (例如 Box::new , 或重組某些資料結構)。這會呼叫 LocalHeap::alloc 。
4. TLAB 耗盡 : 如果 TLAB 滿了， alloc 會呼叫 alloc_slow 。
5. Mutation : alloc_slow 會向 GlobalSegmentManager 請求新 Page，並 將新 Page push 到 heap.pages 中 。

**為什麼這會導致 Hang 或 Crash？**

- Rust 的借用規則 (被 Unsafe 繞過) : 理論上 Rust 禁止在迭代 Vec 的同時對其 push 。但是，因為你的 heap 是透過 UnsafeCell (在 ThreadControlBlock 中) 取得的，並且在 collect 過程中你將 &mut LocalHeap 轉為了 &LocalHeap 傳給 sweep ，你實際上製造了 Aliasing Violation (同時擁有 Mutable 和 Immutable reference)。
- Vector Reallocation : 當 alloc_slow 執行 heap.pages.push(...) 時，如果 Vec 的容量 (capacity) 不足，它會重新配置 (reallocate) 底層的 buffer，並將舊資料 copy 到新位置，然後釋放舊 buffer。
- Dangling Pointer / Infinite Loop : 外層的 for page_ptr in heap.all_pages() 迭代器仍然持有指向 舊 buffer 的指標。
- 迭代器繼續讀取已被釋放的記憶體 (Use-After-Free)。
- 運氣好是 Segfault。
- 運氣不好，它讀到了垃圾數據，這些數據剛好構成了有效的 Page Header 指標，或者導致迭代器狀態錯亂，變成無窮迴圈。

**為什麼 `threads=1` 沒事？**
這可能純粹是巧合或是記憶體配置器的行為差異。在單執行緒測試下，記憶體壓力較小，`Vec` 可能剛好不需要 Reallocate，或者 `drop` 的順序剛好避開了問題。但在多執行緒下，Memory Layout 和 Allocation pattern 改變，觸發了 `Vec` 的擴容。

---

### 修復方案：Snapshotting (快照)

在 Two-Phase Sweep 的第一階段，我們必須對 "當下存在的 Pages" 進行快照，然後迭代這個快照。這樣即使 `drop_fn` 觸發 allocation 並新增 Page，也不會影響我們正在迭代的集合 (新 Page 是 Young 的，不需要被 Sweep)。

請修改 `gc.rs` 中的 `sweep_phase1_finalize` 和 `sweep_large_objects`。

#### 1. 修改 `sweep_phase1_finalize`

```javascript
// gc.rs

fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {
    let mut pending = Vec::new();

    // [FIX]: Snapshot the pages first!
    // Collect pointers into a temporary Vec to avoid iterator invalidation
    // if drop_fn triggers allocation (which modifies heap.pages).
    let all_pages: Vec<_> = heap.all_pages().collect();

    for page_ptr in all_pages {
        unsafe {
             // 這裡的邏輯保持不變
            let header = page_ptr.as_ptr();
            // ...
        }
    }
    pending
}
```

#### 2. 修改 `sweep_large_objects`

同樣的問題也存在於 Large Objects，如果 `drop` 導致 Large Object allocation。

```javascript
// gc.rs

fn sweep_large_objects(heap: &mut LocalHeap, only_young: bool) -> usize {
    // [FIX]: Snapshot large object pages
    // heap.large_object_pages() already returns a Vec, so this is partially safe from
    // invalidation, BUT heap.large_object_pages() iterates self.pages.
    // If we are modifying self.pages inside the loop, we are in trouble.

    // 原本的寫法:
    // let target_pages = heap.large_object_pages();
    // 這行本身是安全的，因為它回傳一個新的 Vec。
    // 但是，原本的程式碼接著做了一個危險的操作：

    // heap.pages.retain(|&p| p != page_ptr); <--- 這是在 mutation!

    // 如果你在迭代 target_pages 的同時修改 heap.pages，這本身沒問題 (因為 target_pages 是 copy)。
    // 但是，如果 drop_fn 導致了 alloc_large -> heap.pages.push，
    // 這可能會干擾我們對 heap.pages 的假設。

    // 讓我們確保邏輯清晰：
    let target_pages = heap.large_object_pages(); // Snapshot

    let mut to_deallocate: Vec<(NonNull<PageHeader>, usize, usize)> = Vec::new();

    for page_ptr in target_pages {
        // ... (檢查 mark, 執行 drop_fn) ...
        // 如果這裡的 drop_fn 觸發 alloc，heap.pages 會改變，但不影響 target_pages 的迭代。
        // 安全。
    }

    // ... (reclaim logic) ...

    // [ADVICE]: 你的 reclaim logic 使用 heap.pages.retain，這在大量 Large Objects 時是 O(N^2)。
    // 建議之後優化，但目前與 Hang 無關。

    let mut reclaimed = 0;
    for (page_ptr, alloc_size, pages_needed) in to_deallocate {
         // ...
    }
    reclaimed
}
```

### 關於 `GlobalSegmentManager` Race 的補充觀察

雖然上述的 Iterator Invalidation 是最可能的兇手，但我必須指出你的 `ThreadLocalHeap::drop` 和 `gc` 之間的潛在 Deadlock 風險，這在多執行緒測試中也很常見。

**情境：**

1. Thread A (正在結束) : 執行 LocalHeap::drop 。它持有了 segment_manager 的 Lock (為了 push orphan pages)。
2. Thread B (Collector) : 正在執行 GC。它執行到 sweep_orphan_pages 。它也試圖獲取 segment_manager 的 Lock。

這本身只是 Contention (競爭)，不是 Deadlock。但是，如果：

- Thread B (Collector) 在持有 segment_manager Lock 的時候，需要等待 Thread A 做某些事 (例如在 Phase 1 drop 時，Thread B 等待 Thread A 的 Channel 回應)。
- 而 Thread A 被 Block 在 segment_manager Lock 上。

這就會造成 Deadlock。這在 `cargo test` 中很常見，因為測試案例常使用 Channel 進行同步。
**解決方案**：在 GC 執行 User Code (`drop_fn`) 時，絕對不能持有任何全域鎖 (`segment_manager` 或 `thread_registry`)。你的實作目前看起來在 Phase 1 沒有持有這些鎖，這是正確的。只要確保 `alloc_slow` 鎖定 `segment_manager` 的範圍僅限於配置期間即可 (目前看來也是正確的)。

### 總結建議

1. 立即修正 sweep_phase1_finalize ：將 heap.all_pages() 收集成一個 Vec (Snapshot) 再進行迭代。這是導致 Hang/Crash 的最直接原因。
2. 檢查 heap.rs 中的 request_gc_handshake ： 在你的 perform_multi_threaded_collect 邏輯中，必須非常小心。如果 request_gc_handshake 回傳 true (代表 active == 1 )，這意味著其他執行緒都處於 Safepoint 或 Inactive。確保你沒有在 "以為只有自己活著" 的情況下，去存取 "其實正在跑且沒停下來" 的執行緒的 Heap。 (你的實作中 request_gc_handshake 似乎是非阻塞的快照，如果它回傳 false (active > 1)，你就 fallback 到 single threaded GC。這意味著如果有多個執行緒在跑，你實際上 沒有 做全域 GC，只做了 Thread-Local GC。這可能導致記憶體洩漏或 OOM，但不應該導致 Hang，除非你的測試依賴 GC 來釋放某些資源) 。

先套用第一點修正，這應該能解決你的 `cargo test` hang 問題。

## 使用者

您對目前的 Two-Phase Sweep 實作品質覺得如何？是少實作Snapshotting，還是錯誤實作 Two-Phase Sweep?

## Gemini

這是一個非常好的問題。作為一個長期研究語言實作與垃圾回收的人（R. Kent Dybvig），我會這樣評價你的實作：

**評價：B- (可運作，但有結構性缺陷)**

**直接回答你的問題：**
這**不仅仅是少實作 Snapshotting** 的問題。雖然 Snapshotting 是導致目前 Hang/Crash 的直接元兇，但你的 Two-Phase Sweep 實作方式在**效率**和**記憶體回收的完整性（Memory Leak）**上存在結構性的設計弱點。

嚴格來說，這是一個「**正確但天真 (Naive) 的 Two-Phase Sweep 實作**」。

讓我分層次為你剖析：

### 1. 致命傷：Snapshotting (安全性問題)

這是我們剛剛討論過的。在迭代過程中允許 `drop_fn` 修改底層容器（`heap.pages`），這是 Undefined Behavior 的溫床。這是必須立即修復的 P0 級錯誤。

- 判定 ：實作錯誤 (Implementation Bug)。

### 2. 結構性弱點：PendingDrop 的重型開銷 (效率問題)

你的 Two-Phase 實作方式非常「重 (Heavyweight)」：

1. Phase 1 : 遍歷所有 Page，將要釋放的物件資訊 ( PendingDrop ) push 到一個 Vec 。
2. Middle : 將 Vec<PendingDrop> 轉換成 HashMap<PageAddr, Vec<Index>> 。
3. Phase 2 : 再次遍歷 Page，根據 HashMap 查表來釋放。

**Dybvig 的評論：**
這種做法產生了大量的暫時性記憶體分配 (`Vec` 和 `HashMap`)。在 GC 壓力大的時候，GC 本身竟然還在狂暴地 `malloc` 記憶體來存儲「要釋放誰」，這非常諷刺且危險（可能導致 OOM）。

**更好的 Two-Phase Sweep 不需要 Side-list：**
通常我們會利用物件 Header 中的 Bit (例如 `allocated` bit 或 `mark` bit 的反轉) 來標記狀態。

- Phase 1 : 掃描 Page。沒被 Mark 的 -> 執行 Drop。 不要 修改 allocated bitmap，也不要移動 Free List。
- Phase 2 : 再次掃描 Page (因為我們有 Page 列表，這很快)。檢查 Bitmap，如果發現「已配置但未標記 (Allocated && !Marked)」的物件（代表剛剛在 Phase 1 被 Drop 了），直接將其加入 Free List 並清除 Allocated bit。

這樣你就不需要 `Vec<PendingDrop>` 也不需要 `HashMap`。原本的空間就能完成工作。

### 3. 嚴重的邏輯漏洞：Orphan Pages 的記憶體洩漏 (完整性問題)

這是我在閱讀你的 `gc.rs` 和 `heap.rs` 時發現最嚴重的設計問題。

請看 `sweep_orphan_pages`：

```rust
// gc.rs
pub fn sweep_orphan_pages() {
    // ...
    manager.orphan_pages.retain(|orphan| unsafe {
        let header = orphan.addr as *mut PageHeader;
        // ...
        // 檢查是否有任何存活物件
        if has_survivors {
            (*header).clear_all_marks();
            true // [Dybvig 注]: 保留整頁
        } else {
            // 回收整頁
            to_reclaim.push((orphan.addr, orphan.size));
            false
        }
    });
    // ...
}
```

**問題在哪？**
如果一個 Orphan Page (原本屬於已死掉的 Thread A) 裡面有 **100 個物件**，其中 **1 個是活的**（被 Thread B 引用），剩下 **99 個是死的**。

你的邏輯是：`has_survivors == true` -> **保留整頁，並且只清除 Mark bit**。

**那剩下的 99 個死物件呢？**

1. 它們的 drop_fn 永遠不會被呼叫 。
2. 它們佔用的記憶體 永遠不會被釋放 (直到那唯一活著的物件也死掉，整頁被回收)。
3. 它們的 Slot 也無法被重複利用（因為 Orphan Page 不在任何 Thread 的 TLAB 或 Free List 來源中）。

這是一個標準的 **Memory Leak**。Orphan Page 變成了「殭屍頁面」，只要有一口氣在，整頁的垃圾都陪葬且無法清理。

**修正建議：**
`sweep_orphan_pages` 必須像正常的 `sweep` 一樣：

1. 掃描 Orphan Page。
2. 對 Page 內未被 Mark 的物件執行 drop 。
3. (可選) 將釋放後的 Slot 加入某個 Free List，或者如果無法重複利用，至少要確保 drop 被執行。

### 4. Mutator 飢餓問題 (Latency 問題)

你的 Two-Phase Sweep 是在 Stop-The-World (或者說 Rendezvous) 期間進行的。
`sweep_phase1_finalize` 執行的是使用者的 `drop` code。這段程式碼的執行時間是**不可控**的。
如果在 GC 暫停期間執行大量複雜的 Destructor，所有執行緒都會卡住。

雖然這在簡單的 GC 實作中是可以接受的，但成熟的 GC (如 Java G1/ZGC 或 Go GC) 通常會將 Finalization 推遲到並發執行緒或獨立的 Finalizer Thread 做，而不是在核心 Sweep 迴圈中做。不過對於目前的專案階段，這一點可以暫時忽略，屬於「品質」而非「錯誤」。

### 總結

你的實作是 **"少實作 Snapshotting (導致 Crash)" + "錯誤實作 Orphan Sweep (導致 Leak)" + "低效實作 Two-Phase (導致 Overhead)"**。

**Dybvig 的建議路徑：**

1. Hotfix (現在做): 加上 Snapshotting，解決 cargo test hang/crash 問題。
2. Optimization (下週做): 移除 Vec<PendingDrop> 和 HashMap 。改寫 sweep_phase2 ，讓它直接再次掃描 Page Header 的 Bitmap 來決定誰該進 Free List。這會讓程式碼更乾淨且更快。
3. Bugfix (重要): 重寫 sweep_orphan_pages 。你必須對 Orphan Page 內的死物件呼叫 drop ，否則你的 GC 會洩漏資源（例如 File Handles, Sockets）。

你目前的程式碼是很好的原型（Prototype），但要成為穩健的 Runtime，還需要打磨這幾個點。
