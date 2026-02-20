//! Test for Bug 3/6: Multi-Page `GcCell` Write Barrier Failure
//!
//! When `GcCell` is allocated in the second (or later) page of a large object,
//! `ptr_to_page_header()` returns invalid header for tail pages. Magic check fails,
//! write barrier is skipped, SATB invariant violated, leading to potential UAF.
//!
//! See: docs/issues/2026-02-18_ISSUES_bug-hunt.md, docs/issues/2026-02-19_ISSUE_bug6_multi_page_gccell_barrier.md

use rudo_gc::cell::GcCell;
use rudo_gc::{collect_full, Gc, Trace};
use std::cell::RefCell;

#[repr(C)]
#[allow(clippy::large_stack_arrays)]
struct Container {
    /// Padding to push `GcCell` beyond first page. 7000 * 8 = 56KB > typical 4KB page.
    _padding: [u64; 7000],
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

    let gc = Gc::new(Container {
        _padding: [0; 7000],
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
