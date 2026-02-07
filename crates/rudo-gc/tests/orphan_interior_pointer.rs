//! Tests for orphan page interior pointer handling.
//!
//! These tests verify that orphan pages with interior pointers are correctly handled.

#![cfg(feature = "test-util")]

use rudo_gc::{collect_full, Gc, Trace};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use rudo_gc::test_util::{clear_test_roots, register_test_root};

#[derive(Trace)]
struct Payload {
    value: u64,
}

#[test]
fn test_orphan_small_object_find_gc_box() {
    clear_test_roots();

    let interior_ptr_addr = Arc::new(AtomicUsize::new(0));

    let handle = thread::spawn({
        let interior_ptr_addr = interior_ptr_addr.clone();
        move || {
            let gc = Gc::new(Payload { value: 0xDEAD_BEEF });
            let ptr = std::ptr::from_ref(&gc.value).cast::<u8>();
            interior_ptr_addr.store(ptr as usize, Ordering::SeqCst);
            gc
        }
    });

    let received_gc = handle.join().unwrap();
    let ptr_addr = interior_ptr_addr.load(Ordering::SeqCst);

    let ptr = ptr_addr as *const u8;
    register_test_root(ptr);

    drop(received_gc);

    collect_full();

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
