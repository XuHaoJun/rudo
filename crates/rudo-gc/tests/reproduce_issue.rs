use rudo_gc::{Gc, Trace, Visitor};
use std::cell::Cell;

// A tracer object to track if it's alive
#[derive(Trace)]
struct Data {
    dropped: Gc<Cell<bool>>,
}

impl Drop for Data {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

pub struct PseudoEffect {
    // Closure captures something, but we cannot see it
    closure: Box<dyn Fn() + 'static>,
}

unsafe impl Trace for PseudoEffect {
    fn trace(&self, visitor: &mut impl Visitor) {
        // Now we fix the bug by conservatively scanning the closure!
        // Safety: Box<dyn Fn()> points to heap memory.
        // We scan the memory region used by the closure.
        let ptr = std::ptr::from_ref::<dyn Fn()>(&*self.closure).cast::<u8>();
        // Correctly calculate the size of the closure struct on the heap.
        let layout = std::alloc::Layout::for_value(&*self.closure);
        let size = layout.size();

        unsafe {
            visitor.visit_region(ptr, size);
        }
    }
}

#[inline(never)]
fn spawn_effect(dropped: Gc<Cell<bool>>) -> Gc<PseudoEffect> {
    let data = Gc::new(Data { dropped });

    let data_clone = Gc::clone(&data);

    // For Miri, the conservative scanner needs pointers to be "exposed"
    // to work when read as raw bytes (usize).
    #[cfg(miri)]
    {
        let ptr = rudo_gc::test_util::internal_ptr(&data_clone);
        let _ = ptr as usize; // Expose the pointer address
    }

    // This Effect lives on the heap (Gc<PseudoEffect>)
    // Its closure captures `data_clone`.
    Gc::new(PseudoEffect {
        closure: Box::new(move || {
            let _ = &data_clone;
        }),
    })
}

#[inline(never)]
fn waste_stack(n: usize) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut arr = [0u64; 64];
    arr.fill(n as u64);
    waste_stack(n - 1) + arr[0]
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_reproduce_closure_trace_hole() {
    let dropped = Gc::new(Cell::new(false));

    let effect = spawn_effect(Gc::clone(&dropped));

    // For Miri, we must explicitly register roots because stack scanning is disabled.
    #[cfg(miri)]
    rudo_gc::test_util::register_test_root(rudo_gc::test_util::internal_ptr(&effect));

    // Clear potential leftovers on the stack
    waste_stack(10);

    // Trigger GC - Use collect_full to be sure
    rudo_gc::collect_full();

    // Check if it's alive.
    // Since `PseudoEffect` NOW traces its closure, the GC should NOT have collected `data`.
    assert!(
        !dropped.get(),
        "Data should NOT have been dropped because closure was traced"
    );

    // Clean up roots for Miri
    #[cfg(miri)]
    rudo_gc::test_util::clear_test_roots();

    // Use effect to keep it alive until here
    drop(effect);
}
