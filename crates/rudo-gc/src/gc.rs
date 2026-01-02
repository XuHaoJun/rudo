//! Mark-Sweep garbage collection algorithm.
//!
//! This module implements the core garbage collection logic using
//! a mark-sweep algorithm with the BiBOP memory layout.

use std::cell::Cell;
use std::ptr::NonNull;

use crate::heap::{with_heap, GlobalHeap, PageHeader, HEAP};
use crate::ptr::GcBox;
use crate::roots::{with_roots, ShadowStack};
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
    pub fn n_gcs_dropped_since_last_collect(&self) -> usize {
        self.n_gcs_dropped
    }

    /// Number of Gc pointers currently existing.
    pub fn n_gcs_existing(&self) -> usize {
        self.n_gcs_existing
    }

    /// Total bytes allocated in heap.
    pub fn heap_size(&self) -> usize {
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
pub fn default_collect_condition(info: &CollectInfo) -> bool {
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
    if condition(&info) {
        collect();
    }
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
    // Reset drop counter
    N_DROPS.with(|n| n.set(0));

    // Phase 1: Clear all marks
    with_heap(|heap| {
        clear_all_marks(heap);
    });

    // Phase 2: Mark all reachable objects
    with_roots(|roots| {
        mark_from_roots(roots);
    });

    // Phase 3: Sweep unmarked objects
    with_heap(|heap| {
        sweep_unmarked(heap);
    });
}

/// Clear all mark bits in the heap.
fn clear_all_marks(heap: &mut GlobalHeap) {
    for page_ptr in heap.all_pages() {
        // SAFETY: Page pointers in the heap are always valid
        unsafe {
            let header = page_ptr.as_ptr();
            (*header).clear_all_marks();
        }
    }
}

/// Mark all objects reachable from roots.
fn mark_from_roots(roots: &ShadowStack) {
    let mut visitor = MarkVisitor;
    
    for root in roots.iter() {
        // SAFETY: Root pointers are valid GcBox pointers
        unsafe {
            mark_object(root, &mut visitor);
        }
    }
}

/// Mark a single object and trace its children.
///
/// # Safety
///
/// The pointer must be a valid GcBox pointer.
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
fn sweep_unmarked(heap: &mut GlobalHeap) {
    // For now, this is a placeholder. In a full implementation, we would:
    // 1. Iterate all pages
    // 2. For each unmarked object, call its destructor
    // 3. Return the memory to the free list
    //
    // The current implementation doesn't actually free memory because we
    // don't have a way to safely call destructors on type-erased objects.
    // This will be addressed in Phase 3 when we add proper type tracking.
    
    let _ = heap; // Suppress unused variable warning
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
