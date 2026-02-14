#![allow(clippy::await_holding_refcell_ref)]

use rudo_gc::{Gc, GcThreadSafeCell, Trace};

#[derive(Trace, Clone)]
struct ThreadSafeSignal {
    value: GcThreadSafeCell<i32>,
}

impl ThreadSafeSignal {
    const fn new(value: i32) -> Self {
        Self {
            value: GcThreadSafeCell::new(value),
        }
    }

    fn get(&self) -> i32 {
        *self.value.borrow()
    }

    fn set(&self, new_value: i32) {
        *self.value.borrow_mut_gen_only() = new_value;
    }
}

#[test]
fn test_gc_thread_safe_cell_basic() {
    let signal = Gc::new(ThreadSafeSignal::new(0));

    assert_eq!(signal.get(), 0);

    signal.set(42);
    assert_eq!(signal.get(), 42);

    signal.set(100);
    assert_eq!(signal.get(), 100);
}

#[test]
fn test_gc_thread_safe_cell_clone() {
    let signal = Gc::new(ThreadSafeSignal::new(10));
    let signal_clone = signal.clone();

    assert_eq!(signal.get(), 10);
    assert_eq!(signal_clone.get(), 10);

    signal.set(20);
    assert_eq!(signal.get(), 20);
    assert_eq!(signal_clone.get(), 20);
}

#[test]
fn test_gc_thread_safe_cell_with_gc_ptrs() {
    #[derive(Trace, Clone)]
    struct SignalWithGc {
        value: GcThreadSafeCell<i32>,
        child: Gc<Gc<i32>>,
    }

    let signal: Gc<SignalWithGc> = Gc::new(SignalWithGc {
        value: GcThreadSafeCell::new(0),
        child: Gc::new(Gc::new(42)),
    });

    assert_eq!(*signal.value.borrow(), 0);
    assert_eq!(**signal.child, 42);

    *signal.value.borrow_mut_gen_only() = 100;
    assert_eq!(*signal.value.borrow(), 100);
}
