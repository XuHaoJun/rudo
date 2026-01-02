//! Mark-Sweep garbage collection algorithm.
//!
//! This module implements the core garbage collection logic using
//! a mark-sweep algorithm with the `BiBOP` memory layout.

use std::cell::Cell;
use std::ptr::NonNull;

use crate::heap::{with_heap, GlobalHeap, PageHeader, HEAP};
use crate::ptr::GcBox;
use crate::trace::{Trace, Visitor};

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
#[must_use]
pub const fn default_collect_condition(info: &CollectInfo) -> bool {
    info.n_gcs_dropped > info.n_gcs_existing
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
    let info = CollectInfo {
        n_gcs_dropped: N_DROPS.with(Cell::get),
        n_gcs_existing: N_EXISTING.with(Cell::get),
        heap_size: HEAP.with(|heap| heap.borrow().total_allocated()),
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

/// Force an immediate garbage collection.
///
/// This function runs the mark-sweep collector synchronously, freeing all
/// unreachable allocations.
pub fn collect() {
    // Reentrancy guard
    if IN_COLLECT.with(Cell::get) {
        return;
    }
    IN_COLLECT.with(|in_collect| in_collect.set(true));

    // Reset drop counter
    N_DROPS.with(|n| n.set(0));

    // Phase 1: Clear all marks
    with_heap(|heap| {
        clear_all_marks(heap);

        // Phase 2: Mark all reachable objects
        mark_from_roots(heap);

        // Phase 3: Sweep unmarked objects
        sweep_unmarked(heap);
    });

    IN_COLLECT.with(|in_collect| in_collect.set(false));
}

/// Clear all mark bits in the heap.
fn clear_all_marks(heap: &GlobalHeap) {
    for page_ptr in heap.all_pages() {
        // SAFETY: Page pointers in the heap are always valid
        unsafe {
            let header = page_ptr.as_ptr();
            (*header).clear_all_marks();
        }
    }
}

/// Mark all objects reachable from roots.
fn mark_from_roots(heap: &GlobalHeap) {
    let mut visitor = MarkVisitor;

    // Use conservative stack scanning to find roots.
    // This replaces the explicit ShadowStack.
    unsafe {
        crate::stack::spill_registers_and_scan(|potential_ptr| {
            if let Some(gc_box_ptr) = crate::heap::find_gc_box_from_ptr(heap, potential_ptr) {
                mark_object(gc_box_ptr, &mut visitor);
            }
        });
    }
}

/// Mark a single object and trace its children.
///
/// # Safety
///
/// The pointer must be a valid `GcBox` pointer.
unsafe fn mark_object(ptr: NonNull<GcBox<()>>, _visitor: &mut MarkVisitor) {
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

        // Trace children
        // Note: We can't easily trace the actual value because we've type-erased it.
        // In a full implementation, we'd store a trace function pointer in the GcBox.
        // For now, this is a limitation that we'll address in Phase 3.
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
// MarkVisitor - Visitor implementation for marking
// ============================================================================

/// A visitor that marks reachable objects.
struct MarkVisitor;

impl Visitor for MarkVisitor {
    fn visit<T: Trace + ?Sized>(&mut self, gc: &crate::Gc<T>) {
        if let Some(ptr) = gc.raw_ptr().as_option() {
            // Mark the object pointed to by this Gc
            // SAFETY: The pointer is valid (we just checked it's not null)
            unsafe {
                mark_object(ptr.cast(), self);
            }

            // Trace the value inside
            if let Some(value) = crate::Gc::try_deref(gc) {
                value.trace(self);
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
        };

        assert_eq!(info.n_gcs_dropped_since_last_collect(), 5);
        assert_eq!(info.n_gcs_existing(), 10);
        assert_eq!(info.heap_size(), 1024);
    }

    #[test]
    fn test_default_collect_condition() {
        // Should not collect when drops < existing
        let info = CollectInfo {
            n_gcs_dropped: 5,
            n_gcs_existing: 10,
            heap_size: 1024,
        };
        assert!(!default_collect_condition(&info));

        // Should collect when drops > existing
        let info = CollectInfo {
            n_gcs_dropped: 15,
            n_gcs_existing: 10,
            heap_size: 1024,
        };
        assert!(default_collect_condition(&info));
    }
}
