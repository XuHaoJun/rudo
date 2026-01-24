//! Conservative scanning of arbitrary heap regions.

use crate::heap::HEAP;
use crate::trace::GcVisitor;

/// Scan a memory region conservatively for potential Gc pointers.
///
/// This function iterates through the given memory region, treating every
/// pointer-aligned word as a potential Gc pointer. If it finds a pointer
/// that looks like it belongs to the Gc heap, it marks the corresponding
/// object as reachable.
///
/// # Safety
///
/// - `region_ptr` must be valid for reading `region_len` bytes.
/// - The scanning process is conservative; it may incorrectly identify
///   integers that happen to look like pointers as valid Gc pointers,
///   causing objects to be kept alive longer than necessary.
pub unsafe fn scan_heap_region_conservatively(
    region_ptr: *const u8,
    region_len: usize,
    visitor: &mut GcVisitor,
) {
    if region_ptr.is_null() || region_len == 0 {
        return;
    }

    // Access the thread-local heap
    let heap_ptr = HEAP.with(|h| h.tcb.heap.get());
    let heap = unsafe { &*heap_ptr };

    let mut current = region_ptr as usize;
    let end = current + region_len;

    // Align the starting pointer
    let align = std::mem::align_of::<usize>();
    if current % align != 0 {
        current += align - (current % align);
    }

    while current + std::mem::size_of::<usize>() <= end {
        // SAFETY: The caller guarantees the region is valid for reading.
        // We load as usize first to avoid Miri issues with loading potentially invalid pointers.
        let potential_addr = unsafe { (current as *const usize).read_unaligned() };
        let potential_ptr = potential_addr as *const u8;

        // Try to find a GcBox at this address
        // SAFETY: find_gc_box_from_ptr performs range and alignment checks.
        if let Some(gc_box) = unsafe { crate::heap::find_gc_box_from_ptr(heap, potential_ptr) } {
            // Mark the object as reachable based on the visitor kind
            unsafe {
                match visitor.kind {
                    crate::trace::VisitorKind::Major => {
                        crate::gc::mark_object(gc_box, visitor);
                    }
                    crate::trace::VisitorKind::Minor => {
                        crate::gc::mark_object_minor(gc_box, visitor);
                    }
                }
            }
        }

        current += align;
    }
}
