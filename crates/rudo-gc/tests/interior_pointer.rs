#![allow(clippy::ptr_as_ptr, clippy::uninlined_format_args)]
//! Tests for interior pointer support (both small and large objects).

use rudo_gc::{collect_full, Gc, Trace};

#[cfg(feature = "test-util")]
use rudo_gc::test_util::{clear_test_roots, internal_ptr, register_test_root};

#[cfg(feature = "test-util")]
macro_rules! root {
    ($gc:expr) => {
        register_test_root(internal_ptr(&$gc))
    };
}

#[cfg(not(feature = "test-util"))]
macro_rules! root {
    ($gc:expr) => {};
}

#[cfg(feature = "test-util")]
macro_rules! clear_roots {
    () => {
        clear_test_roots()
    };
}

#[cfg(not(feature = "test-util"))]
macro_rules! clear_roots {
    () => {};
}

#[derive(Trace)]
struct SmallStruct {
    a: u64,
    b: u64,
    c: u64,
}

#[test]
fn test_small_struct_interior_pointer_basic() {
    let gc = Gc::new(SmallStruct { a: 1, b: 2, c: 3 });

    let ptr_a = std::ptr::from_ref(&gc.a) as *const u8;
    let ptr_b = std::ptr::from_ref(&gc.b) as *const u8;
    let ptr_c = std::ptr::from_ref(&gc.c) as *const u8;

    rudo_gc::heap::with_heap(|heap| unsafe {
        let box_from_a = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_a);
        let box_from_b = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_b);
        let box_from_c = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_c);

        assert!(
            box_from_a.is_some(),
            "Should find GcBox from field 'a' pointer"
        );
        assert!(
            box_from_b.is_some(),
            "Should find GcBox from field 'b' pointer"
        );
        assert!(
            box_from_c.is_some(),
            "Should find GcBox from field 'c' pointer"
        );

        assert_eq!(
            box_from_a, box_from_b,
            "Field 'a' and 'b' should point to same GcBox"
        );
        assert_eq!(
            box_from_b, box_from_c,
            "Field 'b' and 'c' should point to same GcBox"
        );
    });
}

#[derive(Trace)]
struct NestedStruct {
    inner: InnerStruct,
    value: u64,
}

#[derive(Trace)]
struct InnerStruct {
    x: u32,
    y: u32,
    z: u32,
}

#[test]
fn test_nested_struct_interior_pointer() {
    let gc = Gc::new(NestedStruct {
        inner: InnerStruct {
            x: 10,
            y: 20,
            z: 30,
        },
        value: 100,
    });

    let ptr_x = std::ptr::from_ref(&gc.inner.x) as *const u8;
    let ptr_y = std::ptr::from_ref(&gc.inner.y) as *const u8;
    let ptr_value = std::ptr::from_ref(&gc.value) as *const u8;

    rudo_gc::heap::with_heap(|heap| unsafe {
        let box_from_x = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_x);
        let box_from_y = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_y);
        let box_from_value = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_value);

        assert!(
            box_from_x.is_some(),
            "Should find GcBox from nested field 'x'"
        );
        assert!(
            box_from_y.is_some(),
            "Should find GcBox from nested field 'y'"
        );
        assert!(
            box_from_value.is_some(),
            "Should find GcBox from field 'value'"
        );

        assert_eq!(
            box_from_x, box_from_y,
            "Nested fields should point to same GcBox"
        );
        assert_eq!(
            box_from_y, box_from_value,
            "All fields should point to same GcBox"
        );
    });
}

#[derive(Trace)]
struct ArrayStruct {
    data: [u64; 16],
}

#[test]
fn test_array_struct_interior_pointer() {
    let gc = Gc::new(ArrayStruct {
        data: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
    });

    for i in 0..16 {
        let ptr = std::ptr::from_ref(&gc.data[i]) as *const u8;

        rudo_gc::heap::with_heap(|heap| unsafe {
            let found = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr);
            assert!(
                found.is_some(),
                "Should find GcBox from array[{}] pointer",
                i
            );
        });
    }
}

#[derive(Trace)]
struct LargeStruct {
    data: [u64; 256],
}

#[test]
fn test_large_struct_interior_pointer() {
    let gc = Gc::new(LargeStruct { data: [0x55; 256] });

    for i in [0, 50, 100, 150, 200, 255] {
        let ptr = std::ptr::from_ref(&gc.data[i]) as *const u8;

        rudo_gc::heap::with_heap(|heap| unsafe {
            let found = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr);
            assert!(
                found.is_some(),
                "Should find GcBox from large struct data[{}]",
                i
            );
        });
    }
}

#[test]
fn test_interior_pointer_gc_survival() {
    #[derive(Trace)]
    struct Node {
        value: u64,
    }

    let mut interior_ptr: *const u64;
    {
        clear_roots!();
        let gc = Gc::new(Node { value: 42 });
        root!(gc);

        interior_ptr = std::ptr::from_ref(&gc.value);

        collect_full();
        assert_eq!(unsafe { *interior_ptr }, 42);
    }

    #[cfg(feature = "test-util")]
    {
        clear_roots!();
        let gc = Gc::new(Node { value: 42 });
        root!(gc);
        interior_ptr = std::ptr::from_ref(&gc.value);

        collect_full();
        assert_eq!(unsafe { *interior_ptr }, 42);

        clear_roots!();
        let gc = Gc::new(Node { value: 42 });
        root!(gc);
        interior_ptr = std::ptr::from_ref(&gc.value);

        collect_full();
        assert_eq!(unsafe { *interior_ptr }, 42);
    }
}

#[test]
fn test_boundary_pointers() {
    #[derive(Trace)]
    struct TwoField {
        first: u64,
        second: u64,
    }

    let gc = Gc::new(TwoField {
        first: 1,
        second: 2,
    });

    let ptr_to_first = std::ptr::from_ref(&gc.first) as *const u8;
    let ptr_to_second = std::ptr::from_ref(&gc.second) as *const u8;
    let ptr_between = (ptr_to_first as usize + 8) as *const u8;

    rudo_gc::heap::with_heap(|heap| unsafe {
        let box_first = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_to_first);
        let box_second = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_to_second);
        let box_between = rudo_gc::heap::find_gc_box_from_ptr(heap, ptr_between);

        assert!(box_first.is_some());
        assert!(box_second.is_some());
        assert_eq!(box_first, box_second);

        assert_eq!(box_between, box_first);
    });
}
