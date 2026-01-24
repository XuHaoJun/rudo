use rudo_gc::{Gc, GcCell, Trace, Visitor};
use std::sync::atomic::{AtomicBool, Ordering};

// Mimic rvue::Effect layout exactly
pub struct DummyEffect {
    closure: Box<dyn Fn() + 'static>,
    is_dirty: AtomicBool,
    is_running: AtomicBool,
    owner: GcCell<Option<Gc<i32>>>,
    cleanups: GcCell<Vec<Box<dyn FnOnce() + 'static>>>,
}

unsafe impl Trace for DummyEffect {
    fn trace(&self, visitor: &mut impl Visitor) {
        self.owner.trace(visitor);

        let ptr = std::ptr::from_ref::<dyn Fn()>(&*self.closure).cast::<u8>();
        let layout = std::alloc::Layout::for_value(&*self.closure);
        unsafe {
            visitor.visit_region(ptr, layout.size());
        }

        // Manual trace for cleanups
        let cleanups = self.cleanups.borrow();
        for cleanup in cleanups.iter() {
            let func: &dyn FnOnce() = &**cleanup;
            let ptr = std::ptr::from_ref::<dyn FnOnce()>(func).cast::<u8>();
            let layout = std::alloc::Layout::for_value(func);
            unsafe {
                visitor.visit_region(ptr, layout.size());
            }
        }
    }
}

pub struct SignalData {
    pub subscribers: GcCell<Vec<Gc<DummyEffect>>>,
}

unsafe impl Trace for SignalData {
    fn trace(&self, visitor: &mut impl Visitor) {
        self.subscribers.trace(visitor);
    }
}

#[test]
fn test_dummy_effect_segfault() {
    let effect = Gc::new(DummyEffect {
        closure: Box::new(|| {}),
        is_dirty: AtomicBool::new(false),
        is_running: AtomicBool::new(false),
        owner: GcCell::new(None),
        cleanups: GcCell::new(Vec::new()),
    });

    let signal_data = Gc::new(SignalData {
        subscribers: GcCell::new(vec![effect.clone()]),
    });

    rudo_gc::collect_full();
    
    drop(signal_data);
    drop(effect);
}
