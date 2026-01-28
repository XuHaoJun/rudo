//! Test for Bug 1: Premature Removal of Large Objects from Global Map
//!
//! This test verifies that interior pointers to large objects remain valid
//! after the allocating thread terminates and the heap is orphaned.
//!
//! The bug: when `LocalHeap::drop` runs, it removes entries from
//! `GlobalSegmentManager::large_object_map`. This breaks `find_gc_box_from_ptr`
//! for interior pointers during stack scanning.

use rudo_gc::{collect_full, Gc, Trace};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;

#[repr(C)]
#[allow(clippy::large_stack_arrays)]
struct LargeStruct {
    data: [u64; 10000],
}

unsafe impl Trace for LargeStruct {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[test]
#[allow(clippy::large_stack_arrays)]
fn test_thread_termination_with_interior_pointer() {
    let interior_ptr_addr = Arc::new(AtomicUsize::new(0));

    let handle = thread::spawn({
        let interior_ptr_addr = interior_ptr_addr.clone();
        move || {
            let gc = Gc::new(LargeStruct {
                data: [0x42; 10000],
            });
            let ptr = std::ptr::from_ref(&gc.data[8500]).cast::<u8>();
            interior_ptr_addr.store(ptr as usize, Ordering::SeqCst);
            gc
        }
    });

    let gc = handle.join().unwrap();
    let ptr_addr = interior_ptr_addr.load(Ordering::SeqCst);

    drop(gc);
    collect_full();

    unsafe {
        let ptr = ptr_addr as *const u8;
        #[allow(clippy::cast_ptr_alignment)]
        let _value = *ptr.cast::<u64>();
    }
}
