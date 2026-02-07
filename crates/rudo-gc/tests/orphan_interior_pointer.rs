//! Tests that reproduce the orphan page interior pointer bug and verify the fix.
//!
//! When a thread terminates, its `LocalHeap` is dropped and pages become orphaned.
//! If the only root to an object on an orphan page is an interior pointer on
//! another thread (e.g. stored in `AtomicUsize`), `find_gc_box_from_ptr` must resolve
//! it via the global/orphan fallback or the object is reclaimed -> UAF.

#![cfg(feature = "test-util")]

use rudo_gc::{collect_full, Gc, Trace};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;

use rudo_gc::test_util::{clear_test_roots, register_test_root};

#[derive(Trace)]
struct SmallStruct {
    sentinel: u64,
}

#[test]
fn test_orphan_small_object_find_gc_box() {
    clear_test_roots();

    let interior_ptr_addr = Arc::new(AtomicUsize::new(0));

    let handle = thread::spawn({
        let interior_ptr_addr = interior_ptr_addr.clone();
        move || {
            let gc = Gc::new(SmallStruct {
                sentinel: 0xDEAD_BEEF,
            });
            let ptr = std::ptr::from_ref(&gc.sentinel).cast::<u8>();
            interior_ptr_addr.store(ptr as usize, Ordering::SeqCst);
            gc
        }
    });

    let received_gc = handle.join().unwrap();
    let ptr_addr = interior_ptr_addr.load(Ordering::SeqCst);

    // Child thread has terminated; its heap is dropped, pages are orphaned.
    // Register the interior pointer as the only root.
    let ptr = ptr_addr as *const u8;
    register_test_root(ptr);

    // Drop the Gc so the only reference is the registered test root.
    drop(received_gc);

    collect_full();

    // Object must survive via the orphan fallback resolving the test root.
    unsafe {
        #[allow(clippy::cast_ptr_alignment)]
        let value = *ptr.cast::<u64>();
        assert_eq!(
            value, 0xDEAD_BEEF,
            "small object should survive via orphan lookup"
        );
    }

    clear_test_roots();
}

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
fn test_orphan_large_object_find_gc_box() {
    clear_test_roots();

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

    let received_gc = handle.join().unwrap();
    let ptr_addr = interior_ptr_addr.load(Ordering::SeqCst);

    // Child thread has terminated; its heap is dropped, pages are orphaned.
    let ptr = ptr_addr as *const u8;
    register_test_root(ptr);

    drop(received_gc);

    collect_full();

    unsafe {
        #[allow(clippy::cast_ptr_alignment)]
        let value = *ptr.cast::<u64>();
        assert_eq!(
            value, 0x42,
            "large object interior should survive via orphan lookup"
        );
    }

    clear_test_roots();
}
