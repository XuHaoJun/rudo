//! Tests for concurrent SATB barrier correctness.

use rudo_gc::{cell::GcCell, Gc, Trace};
use rudo_gc_derive::GcCell;

#[derive(Trace, GcCell)]
struct Node {
    value: i32,
}

#[test]
fn test_satb_borrow_mut_preserves_old_value() {
    let cell = GcCell::new(Node { value: 42 });
    let gc_cell = Gc::new(cell);

    {
        let mut borrow = gc_cell.borrow_mut();
        assert_eq!(borrow.value, 42);
        borrow.value = 100;
    }

    assert_eq!(gc_cell.borrow().value, 100);
}

#[test]
fn test_satb_capture_gc_ptrs_returns_empty() {
    use rudo_gc::cell::GcCapture;

    let gc = Gc::new(42);
    let slice = gc.capture_gc_ptrs();
    assert!(slice.is_empty());
}

#[test]
fn test_satb_capture_gc_ptrs_into_works() {
    use rudo_gc::cell::GcCapture;

    let gc = Gc::new(42);
    let mut ptrs = Vec::new();
    gc.capture_gc_ptrs_into(&mut ptrs);
    assert_eq!(ptrs.len(), 1);
}
