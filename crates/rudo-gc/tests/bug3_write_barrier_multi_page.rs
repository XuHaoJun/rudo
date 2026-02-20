//! Test for Bug 3/6: Multi-Page `GcCell` Write Barrier Failure
//!
//! When `GcCell` is allocated in the second (or later) page of a large object,
//! `ptr_to_page_header()` returns invalid header for tail pages. Magic check fails,
//! write barrier is skipped, SATB invariant violated, leading to potential UAF.
//!
//! See: docs/issues/2026-02-18_ISSUES_bug-hunt.md, docs/issues/2026-02-19_ISSUE_bug6_multi_page_gccell_barrier.md
//!
//! Platform page sizes (rudo-gc uses allocation_granularity):
//! - Linux: 4KB | macOS Intel: 4KB | macOS ARM (M1/M2): 16KB | Windows: 64KB

use rudo_gc::cell::GcCell;
use rudo_gc::{collect_full, Gc, Trace};
use std::cell::RefCell;

/// Padding size in u64s. 9000 * 8 = 72KB, exceeds 64KB (Windows) and all common Unix page sizes.
const PADDING_U64_COUNT: usize = 9000;

#[repr(C)]
#[allow(clippy::large_stack_arrays)]
struct Container {
    _padding: [u64; PADDING_U64_COUNT],
    cell: GcCell<Gc<RefCell<u32>>>,
}

unsafe impl Trace for Container {
    fn trace(&self, visitor: &mut impl rudo_gc::Visitor) {
        self.cell.trace(visitor);
    }
}

#[test]
#[allow(clippy::large_stack_arrays)]
fn test_multi_page_gccell_write_barrier() {
    let page_size = rudo_gc::heap::page_size();
    let page_mask = rudo_gc::heap::page_mask();
    let padding_bytes = PADDING_U64_COUNT * std::mem::size_of::<u64>();
    assert!(
        padding_bytes > page_size,
        "Padding {padding_bytes} bytes must exceed page_size {page_size} (increase PADDING_U64_COUNT)"
    );

    let gc = Gc::new(Container {
        _padding: [0; PADDING_U64_COUNT],
        cell: GcCell::new(Gc::new(RefCell::new(0))),
    });

    // Verify GcCell is in second page
    let cell_addr = std::ptr::from_ref(&gc.cell) as usize;
    let head_page = (Gc::as_ptr(&gc) as usize) & page_mask;
    let cell_page = cell_addr & page_mask;
    assert_ne!(
        head_page, cell_page,
        "GcCell should be in second page (page_size={page_size})"
    );

    collect_full();

    // Young generation object
    let young_obj = Gc::new(RefCell::new(12345));

    // Write barrier should trigger here, but bug causes it to be skipped
    *gc.cell.borrow_mut() = young_obj;

    collect_full();

    // If bug exists: young_obj may be wrongly collected -> UAF or wrong value
    assert_eq!(
        *gc.cell.borrow().borrow(),
        12345,
        "Young object should survive (write barrier must have recorded OLD->YOUNG ref)"
    );
}
