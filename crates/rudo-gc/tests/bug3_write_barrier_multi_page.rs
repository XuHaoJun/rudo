//! Test for Bug 3: Write Barrier Failure for Multi-Page Large Objects
//!
//! This test verifies that `GcCell` in the second (or later) page of a
//! large object correctly triggers the write barrier.
//!
//! The bug: `ptr_to_page_header()` masks to page boundary. For large objects
//! spanning multiple pages, only the first page has a `PageHeader`. A `GcCell`
//! in tail pages returns garbage data, magic check fails, write barrier skipped.

use rudo_gc::cell::GcCell;
use rudo_gc::{collect_full, Gc, Trace};
use std::cell::RefCell;

#[repr(C)]
#[allow(clippy::large_stack_arrays)]
struct Container {
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
fn test_gccell_write_barrier_in_second_page() {
    let page_size = rudo_gc::heap::page_size();

    let gc = Gc::new(Container {
        _padding: [0; 7000],
        cell: GcCell::new(Gc::new(RefCell::new(0))),
    });

    let cell_addr = std::ptr::from_ref(&gc.cell) as usize;
    let head_page = (Gc::as_ptr(&gc) as usize) & !page_size;
    let cell_page = cell_addr & !page_size;

    assert_ne!(head_page, cell_page, "GcCell should be in second page");

    collect_full();

    let young_obj = Gc::new(RefCell::new(12345));

    *gc.cell.borrow_mut() = young_obj;

    collect_full();

    assert_eq!(
        *gc.cell.borrow().borrow(),
        12345,
        "Young object should survive write barrier"
    );
}
