use rudo_gc::{Gc, Trace, TraceClosure, Visitor};
use std::cell::Cell;

#[derive(Trace)]
struct Data {
    dropped: Gc<Cell<bool>>,
}

impl Drop for Data {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

pub struct EffectWithTraceClosure {
    // Using TraceClosure to wrap the closure and its dependencies
    closure: Gc<TraceClosure<Box<dyn Fn()>, Gc<Data>>>,
}

unsafe impl Trace for EffectWithTraceClosure {
    fn trace(&self, visitor: &mut impl Visitor) {
        self.closure.trace(visitor);
    }
}

#[test]
fn test_trace_closure_preserves_captured_gc() {
    let dropped = Gc::new(Cell::new(false));

    let effect = {
        let data = Gc::new(Data {
            dropped: Gc::clone(&dropped),
        });

        let data_clone = Gc::clone(&data);
        let data_clone_for_tc = Gc::clone(&data_clone);
        // Wrap closure in TraceClosure
        let closure: Box<dyn Fn()> = Box::new(move || {
            let _ = &data_clone;
        });
        let tc = TraceClosure::new(data_clone_for_tc, closure);

        EffectWithTraceClosure {
            closure: Gc::new(tc),
        }
    };

    // For Miri, we must explicitly register roots because stack scanning is disabled.
    #[cfg(miri)]
    rudo_gc::test_util::register_test_root(rudo_gc::test_util::internal_ptr(&effect.closure));

    // Trigger GC
    rudo_gc::collect_full();

    // Since TraceClosure traces its deps, Data should NOT be dropped
    assert!(
        !dropped.get(),
        "Data should NOT have been dropped because TraceClosure traces it"
    );

    // Clean up roots for Miri
    #[cfg(miri)]
    rudo_gc::test_util::clear_test_roots();

    drop(effect);
}
