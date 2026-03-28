use rudo_gc::gc::incremental::IncrementalConfig;
use rudo_gc::test_util::{clear_registers, register_test_root, reset};
use rudo_gc::{collect_full, set_incremental_config, Gc, Trace, Visitor};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default)]
struct NoopVisitor;

impl Visitor for NoopVisitor {
    fn visit<T: Trace>(&mut self, _gc: &Gc<T>) {}

    unsafe fn visit_region(&mut self, _ptr: *const u8, _len: usize) {}
}

struct TraceCounter {
    hits: Rc<Cell<usize>>,
}

// SAFETY: Test helper type has no nested Gc pointers and only updates an external counter.
unsafe impl Trace for TraceCounter {
    fn trace(&self, _visitor: &mut impl Visitor) {
        self.hits.set(self.hits.get() + 1);
    }
}

#[test]
#[should_panic(expected = "already mutably borrowed")]
fn test_refcell_trace_panics_when_mutably_borrowed() {
    let hits = Rc::new(Cell::new(0));
    let cell = RefCell::new(TraceCounter { hits });
    let mut visitor = NoopVisitor;

    let _borrow = cell.borrow_mut();
    cell.trace(&mut visitor);
}

#[derive(Trace)]
struct Leaf {
    marker: Rc<()>,
}

#[derive(Trace)]
struct Holder {
    slot: RefCell<Option<Gc<Leaf>>>,
}

/// Stress experiment for potential premature collection when `RefCell::trace` skips.
///
/// If this test ever fails, it is a strong signal that a live edge was missed during marking.
#[test]
#[should_panic(expected = "already mutably borrowed")]
fn test_collect_full_panics_when_reachable_refcell_is_mutably_borrowed() {
    reset();
    set_incremental_config(IncrementalConfig {
        enabled: true,
        increment_size: 1,
        ..Default::default()
    });

    let marker = Rc::new(());
    let holder = Gc::new(Holder {
        slot: RefCell::new(None),
    });
    register_test_root(Gc::internal_ptr(&holder));

    {
        let leaf = Gc::new(Leaf {
            marker: marker.clone(),
        });
        *holder.slot.borrow_mut() = Some(leaf);
    }
    assert_eq!(Rc::strong_count(&marker), 2);

    let _borrow = holder.slot.borrow_mut();
    unsafe {
        clear_registers();
    }
    collect_full();
}
