//! Mark-Sweep garbage collection algorithm.
//!
//! This module implements the core garbage collection logic using
//! a mark-sweep algorithm with the `BiBOP` memory layout.

use std::cell::Cell;
use std::ptr::NonNull;

use crate::heap::{with_heap, GlobalHeap, PageHeader, HEAP};
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
}

/// Notify that a Gc was created.
pub fn notify_created_gc() {
    N_EXISTING.with(|n| n.set(n.get() + 1));
}

/// Notify that a Gc was dropped.
pub fn notify_dropped_gc() {
    N_DROPS.with(|n| n.set(n.get() + 1));

    // Check if we should collect
    let (total, young, old) = HEAP.with(|heap| {
        let h = heap.borrow();
        (h.total_allocated(), h.young_allocated(), h.old_allocated())
    });

    let info = CollectInfo {
        n_gcs_dropped: N_DROPS.with(Cell::get),
        n_gcs_existing: N_EXISTING.with(Cell::get),
        heap_size: total,
        young_size: young,
        old_size: old,
    };

    let condition = COLLECT_CONDITION.with(Cell::get);
    if condition(&info) && !IN_COLLECT.with(Cell::get) {
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

// ============================================================================
// Mark-Sweep Collection
// ============================================================================

const MINOR_THRESHOLD: usize = 256 * 1024; // 256KB
const MAJOR_THRESHOLD: usize = 10 * 1024 * 1024; // 10MB

/// Perform a garbage collection.
///
/// Decides between Minor and Major collection based on heuristics.
pub fn collect() {
    // Reentrancy guard
    if IN_COLLECT.with(Cell::get) {
        return;
    }
    IN_COLLECT.with(|in_collect| in_collect.set(true));

    // Reset drop counter
    N_DROPS.with(|n| n.set(0));

    with_heap(|heap| {
        let young_size = heap.young_allocated();
        let total_size = heap.total_allocated();

        // Heuristic:
        // - Minor if young > 1MB (or some ratio)
        // - Major if total > 10MB (or some ratio)
        // For now, simple logic:
        // If we have substantial young gen, try Minor.
        // If Old Gen is getting full, do Major.

        if total_size > MAJOR_THRESHOLD {
            collect_major(heap);
        } else if young_size > MINOR_THRESHOLD {
            collect_minor(heap);
        } else {
            // Default to Minor to keep latency low
            collect_minor(heap);
        }
    });

    IN_COLLECT.with(|in_collect| in_collect.set(false));
}

/// Minor Collection: Collect Young Generation only.
fn collect_minor(heap: &mut GlobalHeap) {
    // 1. Mark Phase
    // Roots: Stack + Dirty Old Objects
    mark_minor_roots(heap);

    // 2. Sweep Phase: Only Young Pages
    sweep_young_pages(heap);

    // 3. Promotion Phase
    promote_young_pages(heap);
}

/// Major Collection: Collect Entire Heap.
fn collect_major(heap: &mut GlobalHeap) {
    // 1. Clear ALL marks and dirty bits
    clear_all_marks_and_dirty(heap);

    // 2. Mark Phase: Full Trace
    mark_major_roots(heap);

    // 3. Sweep Phase: All Pages
    sweep_unmarked(heap);

    // 4. Update Generations: All survivors become Old
    promote_all_pages(heap);
}

/// Clear all mark bits and dirty bits in the heap.
fn clear_all_marks_and_dirty(heap: &GlobalHeap) {
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
fn mark_minor_roots(heap: &GlobalHeap) {
    let mut visitor = GcVisitor {
        kind: VisitorKind::Minor,
    };

    // 1. Scan Stack
    unsafe {
        crate::stack::spill_registers_and_scan(|potential_ptr| {
            if let Some(gc_box_ptr) = crate::heap::find_gc_box_from_ptr(heap, potential_ptr) {
                // Only mark if it points to Young object.
                // But mark_object_minor handles the check.
                mark_object_minor(gc_box_ptr, &mut visitor);
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
fn mark_major_roots(heap: &GlobalHeap) {
    let mut visitor = GcVisitor {
        kind: VisitorKind::Major,
    };
    unsafe {
        crate::stack::spill_registers_and_scan(|potential_ptr| {
            if let Some(gc_box_ptr) = crate::heap::find_gc_box_from_ptr(heap, potential_ptr) {
                mark_object(gc_box_ptr, &mut visitor);
            }
        });
    }
}

/// Mark object for Minor GC.
unsafe fn mark_object_minor(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let page_addr = (ptr_addr as usize) & crate::heap::PAGE_MASK;
    let header = page_addr as *mut PageHeader;

    // SAFETY: We're inside an unsafe fn, but unsafe_op_in_unsafe_fn requires block
    unsafe {
        if (*header).magic != crate::heap::MAGIC_GC_PAGE {
            return;
        }

        // IF OLD GENERATION: STOP.
        if (*header).generation > 0 {
            return;
        }

        let block_size = (*header).block_size as usize;
        let header_size = PageHeader::header_size(block_size);
        let data_start = page_addr + header_size;
        let offset = ptr_addr as usize - data_start;
        let index = offset / block_size;

        if (*header).is_marked(index) {
            return;
        }

        (*header).set_mark(index);

        // Trace children using value's trace_fn
        ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), visitor);
    }
}

/// Sweep Young Pages.
fn sweep_young_pages(heap: &GlobalHeap) {
    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            // Skip Old Pages
            if (*header).generation > 0 {
                continue;
            }
            if (*header).flags & 0x01 != 0 {
                continue;
            }

            // Sweep logic using shared helper
            copy_sweep_logic(header);
        }
    }
}

/// Shared sweep logic (inlined from `sweep_segment_pages` for now to avoid borrow issues)
unsafe fn copy_sweep_logic(header: *mut PageHeader) {
    // SAFETY: unsafe_op_in_unsafe_fn
    unsafe {
        let block_size = (*header).block_size as usize;
        let obj_count = (*header).obj_count as usize;
        let header_size = PageHeader::header_size(block_size);
        let page_addr = header.cast::<u8>();

        let mut free_head: Option<u16> = None;
        for i in (0..obj_count).rev() {
            if !(*header).is_marked(i) {
                let obj_ptr = page_addr.add(header_size + (i * block_size));
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                ((*gc_box_ptr).drop_fn)(obj_ptr);

                #[allow(clippy::cast_possible_truncation)]
                let idx = i as u16;
                #[allow(clippy::cast_ptr_alignment)]
                let obj_cast = obj_ptr.cast::<Option<u16>>();
                *obj_cast = free_head;
                free_head = Some(idx);

                N_EXISTING.with(|n| n.set(n.get().saturating_sub(1)));
            }
        }
        (*header).free_list_head = free_head;
    }
}

/// Promote Young Pages to Old Generation.
fn promote_young_pages(heap: &mut GlobalHeap) {
    let mut promoted_bytes = 0;

    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            if (*header).generation == 0 {
                // Determine if page has survivors
                let mut has_survivors = false;
                let mut survivors_count = 0;

                for i in 0..4 {
                    let bits = (*header).mark_bitmap[i];
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

/// Promote ALL pages (after Major GC).
fn promote_all_pages(heap: &GlobalHeap) {
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
    let page_addr = (ptr_addr as usize) & crate::heap::PAGE_MASK;
    let header = page_addr as *mut PageHeader;

    // SAFETY: We're inside an unsafe fn and caller guarantees ptr is valid
    unsafe {
        // Validate this is a GC page
        if (*header).magic != crate::heap::MAGIC_GC_PAGE {
            return;
        }

        // Calculate object index
        let block_size = (*header).block_size as usize;
        let header_size = PageHeader::header_size(block_size);
        let data_start = page_addr + header_size;
        let offset = ptr_addr as usize - data_start;
        let index = offset / block_size;

        // Check if already marked
        if (*header).is_marked(index) {
            return;
        }

        // Mark this object
        (*header).set_mark(index);

        // Trace children using value's trace_fn
        ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), visitor);
    }
}

/// Sweep all unmarked objects.
///
/// This includes both regular segments and Large Object Space (LOS).
fn sweep_unmarked(heap: &mut GlobalHeap) {
    // Phase 1: Sweep regular segment pages
    sweep_segment_pages(heap);

    // Phase 2: Sweep Large Object Space
    sweep_large_objects(heap);
}

/// Sweep pages in regular segments.
#[allow(clippy::cast_ptr_alignment)]
fn sweep_segment_pages(heap: &GlobalHeap) {
    for page_ptr in heap.all_pages() {
        // SAFETY: Page pointers from all_pages are always valid
        unsafe {
            let header = page_ptr.as_ptr();

            // Skip large objects (handled separately)
            if (*header).flags & 0x01 != 0 {
                continue;
            }

            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);
            let page_addr = header.cast::<u8>();

            // Build free list from unmarked objects
            let mut free_head: Option<u16> = None;
            for i in (0..obj_count).rev() {
                if !(*header).is_marked(i) {
                    // Object is unmarked - it is garbage!
                    let obj_ptr = page_addr.add(header_size + (i * block_size));
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                    // 1. Call the destructor
                    // SAFETY: Unmarked objects are unreachable from roots.
                    // We call the drop_fn which was initialized in Gc::new.
                    ((*gc_box_ptr).drop_fn)(obj_ptr);

                    // 2. Add to free list
                    #[allow(clippy::cast_possible_truncation)]
                    let idx = i as u16;

                    // Store the current free_head in the object's memory
                    // SAFETY: We just dropped the value, so we can use its memory
                    *(obj_ptr.cast::<Option<u16>>()) = free_head;
                    free_head = Some(idx);

                    // 3. Update statistics
                    N_EXISTING.with(|n| n.set(n.get().saturating_sub(1)));
                }
            }

            // Update free list head
            (*header).free_list_head = free_head;
        }
    }
}

/// Sweep Large Object Space.
///
/// Large objects that are unmarked should be deallocated entirely.
fn sweep_large_objects(heap: &mut GlobalHeap) {
    // We need to iterate and potentially remove items from large_objects
    let mut i = 0;
    while i < heap.large_object_pages().len() {
        let page_ptr = heap.large_object_pages()[i];
        // SAFETY: Large object pointers are valid
        unsafe {
            let header = page_ptr.as_ptr();

            if !(*header).is_marked(0) {
                // The object is unreachable - deallocate it
                let block_size = (*header).block_size as usize;
                let header_size = std::mem::size_of::<PageHeader>();
                let obj_ptr = header.cast::<u8>().add(header_size);
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                // 1. Call the destructor
                ((*gc_box_ptr).drop_fn)(obj_ptr);

                // 2. Deallocate the pages
                // Note: Large objects are allocated with Layout::from_size_align
                let total_size = PageHeader::header_size(block_size) + block_size;
                let pages_needed = total_size.div_ceil(crate::heap::PAGE_SIZE);
                let alloc_size = pages_needed * crate::heap::PAGE_SIZE;
                let layout =
                    std::alloc::Layout::from_size_align(alloc_size, crate::heap::PAGE_SIZE)
                        .expect("Invalid large object layout");

                std::alloc::dealloc(header.cast::<u8>(), layout);

                // 3. Remove from the heap's list
                heap.large_object_pages_mut().swap_remove(i);

                // 4. Update statistics
                N_EXISTING.with(|n| n.set(n.get().saturating_sub(1)));

                // Don't increment i because we swap_removed
                continue;
            }
        }
        i += 1;
    }
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

        let mut keep = Vec::new();
        for _ in 0..100 {
            keep.push(crate::Gc::new(42));
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
                    (*page).generation,
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
    }

    #[test]
    fn test_write_barrier() {
        use crate::cell::GcCell;

        // 1. Create Old Gen object
        let old_cell = crate::Gc::new(GcCell::new(None));

        // Force promotion
        crate::heap::with_heap(collect_minor);

        {
            let ptr = crate::Gc::as_ptr(&old_cell);
            unsafe {
                let page = crate::heap::ptr_to_page_header(ptr.cast());
                assert_eq!((*page).generation, 1);
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
                assert!((*page).is_dirty(idx), "Write barrier should set dirty bit");
            }
        }

        // 4. Drop strong ref to young, keep only via old
        drop(young);

        // 5. Collect Minor
        crate::heap::with_heap(collect_minor);

        // 6. Verify Young object survived (accessible via old_cell)
        assert_eq!(**old_cell.borrow().as_ref().unwrap(), 100);
    }
}
