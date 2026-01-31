//! Tests for concurrent marking and sweeping to verify the memory fence fix.
//!
//! These tests exercise the race condition where a sweeping thread could clear
//! mark bits before a slow marking thread's writes propagate, causing live
//! objects to be incorrectly swept.

#![allow(clippy::used_underscore_binding, clippy::cast_sign_loss, dead_code)]

use rudo_gc::{collect, Gc};

#[derive(Clone, Copy, Debug)]
struct TracedValue(usize);

unsafe impl rudo_gc::Trace for TracedValue {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {
        // Nothing to trace - we're tracking the value itself
    }
}

#[test]
fn test_live_objects_survive_many_collections() {
    let gc_values: Vec<Gc<TracedValue>> = (0..50).map(|i| Gc::new(TracedValue(i))).collect();

    // Store original pointers
    let original_ptrs: Vec<_> = gc_values.iter().map(|gc| Gc::as_ptr(gc).cast()).collect();

    for _ in 0..50 {
        let gc_handles: Vec<Gc<u32>> = (0..20).map(|i| Gc::new(i as u32)).collect();
        collect();
        drop(gc_handles);
    }

    assert_eq!(gc_values.len(), 50);
    for (idx, gc) in gc_values.iter().enumerate() {
        let current_ptr: *const std::ffi::c_void = Gc::as_ptr(gc).cast();
        assert_eq!(
            current_ptr, original_ptrs[idx],
            "Object {idx} was collected and reallocated (ptr changed from {:?} to {:?})",
            original_ptrs[idx], current_ptr
        );
    }
}

#[test]
fn test_nested_collections_preserve_live() {
    let outer = Gc::new(TracedValue(42));
    let original_ptr = Gc::as_ptr(&outer).cast();

    for _ in 0..10 {
        let _temp = Gc::new(999u32);
        collect();
        let current_ptr: *const std::ffi::c_void = Gc::as_ptr(&outer).cast();
        assert_eq!(
            current_ptr, original_ptr,
            "Outer object was collected during nested collections"
        );
    }

    let final_ptr: *const std::ffi::c_void = Gc::as_ptr(&outer).cast();
    assert_eq!(final_ptr, original_ptr);
}

#[test]
fn test_many_collections_with_various_sizes() {
    let gc_refs: Vec<Gc<TracedValue>> = (0..100).map(|i| Gc::new(TracedValue(i))).collect();

    // Store original pointers for all 100 objects
    let original_ptrs: Vec<_> = gc_refs.iter().map(|gc| Gc::as_ptr(gc).cast()).collect();

    for iteration in 0..20 {
        // Create temporaries that will be collected
        for _ in 0..10 {
            let _temp = Gc::new(TracedValue(9999));
        }

        collect();

        // Check all original objects - they should all survive
        for (idx, gc) in gc_refs.iter().enumerate() {
            let current_ptr: *const std::ffi::c_void = Gc::as_ptr(gc).cast();
            assert_eq!(
                current_ptr, original_ptrs[idx],
                "Pre-existing object {idx} was collected at iteration {iteration}"
            );
        }
    }
}

#[test]
fn test_mixed_allocations_and_collections() {
    let survivors: Vec<Gc<TracedValue>> = (0..30).map(|i| Gc::new(TracedValue(i))).collect();

    let original_ptrs: Vec<_> = survivors.iter().map(|gc| Gc::as_ptr(gc).cast()).collect();

    for round in 0..15 {
        let _temporaries: Vec<Gc<u32>> = (0..50).map(|i| Gc::new((i * 1000) as u32)).collect();

        collect();

        drop(_temporaries);

        for (idx, gc) in survivors.iter().enumerate() {
            let current_ptr: *const std::ffi::c_void = Gc::as_ptr(gc).cast();
            assert_eq!(
                current_ptr, original_ptrs[idx],
                "Survivor {idx} was collected in round {round}"
            );
        }
    }
}
