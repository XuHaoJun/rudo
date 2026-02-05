use rudo_gc::{cell::GcCell, Gc, Trace};
use rudo_gc_derive::GcCell;

#[derive(Trace, GcCell)]
struct Inner {
    value: i32,
}

#[derive(Trace, GcCell)]
struct Container<T: Trace + 'static> {
    gc_cell: GcCell<Option<Gc<T>>>,
}

#[test]
fn test_gccell_option_gc_generic() {
    let container: Container<Inner> = Container {
        gc_cell: GcCell::new(None),
    };
    assert!(container.gc_cell.borrow().is_none());
}
