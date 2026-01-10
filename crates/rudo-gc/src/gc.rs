//! Mark-Sweep garbage collection algorithm.
//!
//! This module implements the core garbage collection logic using
//! a mark-sweep algorithm with the `BiBOP` memory layout.

use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;

use crate::heap::{LocalHeap, PageHeader};
use crate::ptr::GcBox;
use crate::trace::{GcVisitor, Trace, Visitor, VisitorKind};

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
        crate::heap::GC_REQUESTED.store(false, Ordering::Relaxed);
        perform_single_threaded_collect();
    }
}

/// Perform single-threaded collection (fallback for tests).
fn perform_single_threaded_collect() {
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

    IN_COLLECT.with(|in_collect| in_collect.set(false));
}

/// Perform collection as the collector thread.
fn perform_multi_threaded_collect() {
    IN_COLLECT.with(|in_collect| in_collect.set(true));

    let start = std::time::Instant::now();
    let before_bytes = crate::heap::HEAP.with(|h| unsafe { &*h.tcb.heap.get() }.total_allocated());

    // Reset drop counter
    N_DROPS.with(|n| n.set(0));

    let mut objects_reclaimed = 0;

    // Determine collection type based on current thread's heap
    let total_size = crate::heap::HEAP.with(|h| {
        let heap = unsafe { &*h.tcb.heap.get() };
        heap.total_allocated()
    });

    let tcbs = crate::heap::get_all_thread_control_blocks();

    if total_size > MAJOR_THRESHOLD {
        for tcb in &tcbs {
            unsafe {
                objects_reclaimed += collect_major_multi(&mut *tcb.heap.get());
            }
        }
    } else {
        for tcb in &tcbs {
            unsafe {
                objects_reclaimed += collect_minor_multi(&mut *tcb.heap.get());
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
        crate::heap::GC_REQUESTED.store(false, Ordering::Relaxed);
        perform_single_threaded_collect_full();
    }
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

    let tcbs = crate::heap::get_all_thread_control_blocks();
    for tcb in &tcbs {
        unsafe {
            objects_reclaimed += collect_major_multi(&mut *tcb.heap.get());
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
fn collect_minor_multi(heap: &mut LocalHeap) -> usize {
    mark_minor_roots_multi(heap);
    let reclaimed = sweep_segment_pages(heap, true);
    let reclaimed_large = sweep_large_objects(heap, true);
    promote_young_pages(heap);
    reclaimed + reclaimed_large
}

/// Major collection for a heap in multi-threaded context.
fn collect_major_multi(heap: &mut LocalHeap) -> usize {
    clear_all_marks_and_dirty(heap);
    mark_major_roots_multi(heap);
    let reclaimed = sweep_segment_pages(heap, false);
    let reclaimed_large = sweep_large_objects(heap, false);
    promote_all_pages(heap);
    reclaimed + reclaimed_large
}

/// Mark roots from all threads' stacks for Minor GC.
fn mark_minor_roots_multi(heap: &mut LocalHeap) {
    let mut visitor = GcVisitor {
        kind: VisitorKind::Minor,
    };

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

    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            if (*header).generation == 0 {
                continue;
            }
            if (*header).flags & 0x01 != 0 {
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
fn mark_major_roots_multi(heap: &mut LocalHeap) {
    let mut visitor = GcVisitor {
        kind: VisitorKind::Major,
    };

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
            if (*header).flags & 0x01 != 0 {
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
fn sweep_segment_pages(heap: &LocalHeap, only_young: bool) -> usize {
    let mut total_reclaimed = 0;
    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();

            // Skip large objects (handled separately)
            if (*header).flags & 0x01 != 0 {
                continue;
            }

            // If we are only sweeping young gen, skip old objects
            if only_young && (*header).generation > 0 {
                continue;
            }

            total_reclaimed += copy_sweep_logic(header);
        }
    }
    total_reclaimed
}

/// Shared sweep logic (inlined from `sweep_segment_pages` for now to avoid borrow issues)
unsafe fn copy_sweep_logic(header: *mut PageHeader) -> usize {
    let mut reclaimed = 0;
    // SAFETY: unsafe_op_in_unsafe_fn
    unsafe {
        let block_size = (*header).block_size as usize;
        let obj_count = (*header).obj_count as usize;
        let header_size = PageHeader::header_size(block_size);
        let page_addr = header.cast::<u8>();

        let mut free_head: Option<u16> = None;
        for i in (0..obj_count).rev() {
            if (*header).is_marked(i) {
                // Object is reachable - keep it and clear mark for next collection
                (*header).clear_mark(i);
            } else if (*header).is_allocated(i) {
                // Object is unreachable but was allocated - potentially reclaim
                let obj_ptr = page_addr.add(header_size + (i * block_size));
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                let weak_count = (*gc_box_ptr).weak_count();
                if weak_count > 0 {
                    // There are weak references - drop the value but keep the GcBox allocation
                    if !(*gc_box_ptr).is_value_dead() {
                        ((*gc_box_ptr).drop_fn)(obj_ptr);
                        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                        (*gc_box_ptr).set_dead();
                    }
                } else {
                    // No weak references - fully reclaim the slot
                    ((*gc_box_ptr).drop_fn)(obj_ptr);

                    (*header).clear_allocated(i);
                    #[allow(clippy::cast_possible_truncation)]
                    let idx = i as u16;
                    #[allow(clippy::cast_ptr_alignment)]
                    let obj_cast = obj_ptr.cast::<Option<u16>>();
                    *obj_cast = free_head;
                    free_head = Some(idx);

                    reclaimed += 1;
                    N_EXISTING.with(|n| n.set(n.get().saturating_sub(1)));
                }
            } else {
                // Slot was already free - add it back to the free list
                let obj_ptr = page_addr.add(header_size + (i * block_size));
                #[allow(clippy::cast_possible_truncation)]
                let idx = i as u16;
                #[allow(clippy::cast_ptr_alignment)]
                let obj_cast = obj_ptr.cast::<Option<u16>>();
                *obj_cast = free_head;
                free_head = Some(idx);
            }
        }
        (*header).free_list_head = free_head;
    }
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
    let mut reclaimed = 0;

    // Collect large object pages once to avoid re-scanning heap.pages in every iteration.
    // This also avoids UB by not re-inspecting deallocated pages during the loop.
    let target_pages = heap.large_object_pages(); // .large_object_pages() already returns an owned Vec

    for page_ptr in target_pages {
        // SAFETY: Large object pointers were valid at start of sweep.
        unsafe {
            let header = page_ptr.as_ptr();

            // If we are only sweeping young gen, skip old objects
            if only_young && (*header).generation > 0 {
                continue;
            }

            if !(*header).is_marked(0) {
                // The object is unreachable - check for weak references
                let block_size = (*header).block_size as usize;
                let header_size = (*header).header_size as usize;
                let obj_ptr = header.cast::<u8>().add(header_size);
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                // Check if there are weak references
                let weak_count = (*gc_box_ptr).weak_count();

                if weak_count > 0 {
                    // There are weak references - drop the value but keep the allocation
                    if !(*gc_box_ptr).is_value_dead() {
                        // Only drop if not already dropped
                        ((*gc_box_ptr).drop_fn)(obj_ptr);
                        // Mark as dead by setting drop_fn to no_op
                        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                        (*gc_box_ptr).set_dead();
                    }
                    continue;
                }

                // No weak references - fully deallocate

                // 1. Call the destructor
                ((*gc_box_ptr).drop_fn)(obj_ptr);

                // 2. Prepare deallocation info
                let total_size = header_size + block_size;
                let pages_needed = total_size.div_ceil(crate::heap::PAGE_SIZE);
                let alloc_size = pages_needed * crate::heap::PAGE_SIZE;
                let header_addr = header as usize;

                // 3. Remove from the heap's primary list BEFORE deallocating
                heap.pages.retain(|&p| p != page_ptr);

                // 4. Remove pages from the map
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

                // 5. Deallocate the memory
                // SAFETY: This was allocated via sys_alloc::Mmap.
                sys_alloc::Mmap::from_raw(header.cast::<u8>(), alloc_size);

                // 6. Update statistics
                reclaimed += 1;
                N_EXISTING.with(|n| n.set(n.get().saturating_sub(1)));
            }
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
        let _x = crate::Gc::new(42);
        crate::collect_full();

        let metrics = crate::last_gc_metrics();
        assert!(metrics.total_collections > 0, "No metrics recorded!");
        assert!(metrics.bytes_surviving > 0, "No surviving bytes recorded!");
        assert_eq!(
            metrics.collection_type,
            crate::metrics::CollectionType::Major
        );
    }

    #[test]
    fn test_multi_threaded_gc_handshake() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        clear_test_roots();

        let num_threads = 4;
        let objects_per_thread = 50;

        let barrier = Arc::new(Barrier::new(num_threads));
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let survivor_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let results: Vec<usize> = (0..num_threads)
            .map(|i| {
                let barrier = barrier.clone();
                let completed = completed.clone();
                let survivor_count = survivor_count.clone();

                thread::spawn(move || {
                    barrier.wait();

                    let mut local_survivors = 0;

                    for j in 0..objects_per_thread {
                        let val = i * 1000 + j;
                        let gc_val = crate::Gc::new(val);

                        if j % 2 == 0 {
                            register_test_root(crate::ptr::Gc::internal_ptr(&gc_val));
                            local_survivors += 1;
                        }

                        if j % 10 == 0 {
                            crate::safepoint();
                        }
                    }

                    survivor_count.fetch_add(local_survivors, std::sync::atomic::Ordering::SeqCst);
                    completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                    local_survivors
                })
            })
            .map(|h| h.join().unwrap())
            .collect();

        let total_survivors: usize = results.iter().sum();

        assert_eq!(
            completed.load(std::sync::atomic::Ordering::SeqCst),
            num_threads,
            "All threads should complete"
        );

        crate::collect_full();

        let metrics = crate::last_gc_metrics();
        assert!(
            metrics.total_collections > 0,
            "Collection should have occurred"
        );

        let expected_survivors = total_survivors;
        let actual_survivors = survivor_count.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            actual_survivors, expected_survivors,
            "All registered roots should survive collection"
        );

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
}
