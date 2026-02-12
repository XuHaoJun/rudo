//! Tests verifying that `Drop::drop()` is called at the correct pointer address
//! across various sizes and alignments.
//!
//! This is critical for memory safety: if the GC drops an object at the wrong
//! address, we have use-after-free or double-free bugs. These tests verify that
//! the pointer passed to `Drop::drop()` matches the pointer returned by
//! `Gc::as_ptr()` during allocation.
//!
//! Inspired by gc-arena's `test_layouts` pattern.

use std::sync::atomic::{AtomicPtr, Ordering};

use rudo_gc::{collect_full, Gc, Trace};

thread_local! {
    static DROPPED_PTR: AtomicPtr<u8> = const { AtomicPtr::new(std::ptr::null_mut()) };
}

fn reset_dropped_ptr() {
    DROPPED_PTR.with(|ptr| ptr.store(std::ptr::null_mut(), Ordering::SeqCst));
}

fn get_dropped_ptr() -> *const u8 {
    DROPPED_PTR.with(|ptr| ptr.load(Ordering::SeqCst))
}

#[repr(C)]
struct DropVerifier<T> {
    _data: T,
}

impl<T> Drop for DropVerifier<T> {
    fn drop(&mut self) {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        DROPPED_PTR.with(|atomic| atomic.store(ptr.cast_mut(), Ordering::SeqCst));
    }
}

unsafe impl<T: 'static> Trace for DropVerifier<T> {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

macro_rules! test_layout {
    ($name:ident, size=$size:literal, align=$align:literal) => {
        #[allow(dead_code)]
        #[test]
        fn $name() {
            #[repr(align($align))]
            struct AlignedData(#[allow(dead_code)] [u8; $size]);

            reset_dropped_ptr();

            let gc = Gc::new(DropVerifier {
                _data: AlignedData([0u8; $size]),
            });
            let alloc_ptr = Gc::as_ptr(&gc).cast::<u8>();

            drop(gc);
            collect_full();

            let dropped_ptr = get_dropped_ptr();
            if $size == 0 {
                assert!(
                    !dropped_ptr.is_null(),
                    "size={}, align={}: drop should be called",
                    $size,
                    $align
                );
            } else {
                assert_eq!(
                    alloc_ptr, dropped_ptr,
                    "size={}, align={}: drop called at wrong address - alloc={:?}, dropped={:?}",
                    $size, $align, alloc_ptr, dropped_ptr
                );
            }
        }
    };
}

test_layout!(test_size_8_align_1, size = 8, align = 1);
test_layout!(test_size_8_align_2, size = 8, align = 2);
test_layout!(test_size_8_align_4, size = 8, align = 4);
test_layout!(test_size_8_align_8, size = 8, align = 8);
test_layout!(test_size_8_align_16, size = 8, align = 16);
test_layout!(test_size_8_align_32, size = 8, align = 32);

test_layout!(test_size_16_align_1, size = 16, align = 1);
test_layout!(test_size_16_align_2, size = 16, align = 2);
test_layout!(test_size_16_align_4, size = 16, align = 4);
test_layout!(test_size_16_align_8, size = 16, align = 8);
test_layout!(test_size_16_align_16, size = 16, align = 16);
test_layout!(test_size_16_align_32, size = 16, align = 32);

test_layout!(test_size_32_align_1, size = 32, align = 1);
test_layout!(test_size_32_align_2, size = 32, align = 2);
test_layout!(test_size_32_align_4, size = 32, align = 4);
test_layout!(test_size_32_align_8, size = 32, align = 8);
test_layout!(test_size_32_align_16, size = 32, align = 16);
test_layout!(test_size_32_align_32, size = 32, align = 32);

test_layout!(test_size_64_align_1, size = 64, align = 1);
test_layout!(test_size_64_align_2, size = 64, align = 2);
test_layout!(test_size_64_align_4, size = 64, align = 4);
test_layout!(test_size_64_align_8, size = 64, align = 8);
test_layout!(test_size_64_align_16, size = 64, align = 16);
test_layout!(test_size_64_align_32, size = 64, align = 32);

test_layout!(test_size_0_align_1, size = 0, align = 1);
test_layout!(test_size_0_align_2, size = 0, align = 2);
test_layout!(test_size_0_align_4, size = 0, align = 4);
test_layout!(test_size_0_align_8, size = 0, align = 8);
test_layout!(test_size_0_align_16, size = 0, align = 16);
test_layout!(test_size_0_align_32, size = 0, align = 32);

#[test]
fn test_basic_pointer_identity() {
    #[repr(C)]
    struct SimpleData {
        a: u64,
        b: u64,
    }

    reset_dropped_ptr();

    let gc = Gc::new(DropVerifier {
        _data: SimpleData { a: 1, b: 2 },
    });
    let alloc_ptr = Gc::as_ptr(&gc).cast::<u8>();

    drop(gc);
    collect_full();

    let dropped_ptr = get_dropped_ptr();
    assert_eq!(
        alloc_ptr, dropped_ptr,
        "basic u128 struct: drop called at wrong address - alloc={alloc_ptr:?}, dropped={dropped_ptr:?}",
    );
}

#[test]
fn test_multiple_objects_drop_order() {
    #[repr(C)]
    struct Data16([u8; 16]);

    #[repr(C)]
    struct Data32([u8; 32]);

    reset_dropped_ptr();

    let gc1 = Gc::new(DropVerifier {
        _data: Data16([1; 16]),
    });
    let gc2 = Gc::new(DropVerifier {
        _data: Data16([2; 16]),
    });
    let gc3 = Gc::new(DropVerifier {
        _data: Data32([3; 32]),
    });
    let gc4 = Gc::new(DropVerifier {
        _data: Data32([4; 32]),
    });

    let ptr1 = Gc::as_ptr(&gc1).cast::<u8>();
    let ptr2 = Gc::as_ptr(&gc2).cast::<u8>();
    let ptr3 = Gc::as_ptr(&gc3).cast::<u8>();
    let ptr4 = Gc::as_ptr(&gc4).cast::<u8>();

    drop(gc2);
    collect_full();
    let dropped = get_dropped_ptr();
    assert_eq!(ptr2, dropped, "gc2 (dropped first)");

    drop(gc4);
    collect_full();
    let dropped = get_dropped_ptr();
    assert_eq!(ptr4, dropped, "gc4 (dropped second)");

    drop(gc1);
    collect_full();
    let dropped = get_dropped_ptr();
    assert_eq!(ptr1, dropped, "gc1 (dropped third)");

    drop(gc3);
    collect_full();
    let dropped = get_dropped_ptr();
    assert_eq!(ptr3, dropped, "gc3 (dropped last)");
}

#[test]
fn test_mixed_sizes_and_alignments() {
    #[repr(align(1))]
    struct Align1(#[allow(dead_code)] [u8; 1]);

    #[repr(align(16))]
    struct Align16(#[allow(dead_code)] [u8; 16]);

    #[repr(align(32))]
    struct Align32(#[allow(dead_code)] [u8; 32]);

    reset_dropped_ptr();

    let gc_small = Gc::new(DropVerifier { _data: Align1([0]) });
    let gc_medium = Gc::new(DropVerifier {
        _data: Align16([0; 16]),
    });
    let gc_large = Gc::new(DropVerifier {
        _data: Align32([0; 32]),
    });

    let ptr_small = Gc::as_ptr(&gc_small).cast::<u8>();
    let ptr_medium = Gc::as_ptr(&gc_medium).cast::<u8>();
    let ptr_large = Gc::as_ptr(&gc_large).cast::<u8>();

    drop(gc_small);
    collect_full();
    let dropped = get_dropped_ptr();
    assert_eq!(ptr_small, dropped, "small (align=1, size=1)");

    drop(gc_medium);
    collect_full();
    let dropped = get_dropped_ptr();
    assert_eq!(ptr_medium, dropped, "medium (align=16, size=16)");

    drop(gc_large);
    collect_full();
    let dropped = get_dropped_ptr();
    assert_eq!(ptr_large, dropped, "large (align=32, size=32)");
}

#[test]
fn test_zst_pointer_identity() {
    reset_dropped_ptr();

    let gc = Gc::new(DropVerifier::<()> { _data: () });
    let alloc_ptr: *const DropVerifier<()> = Gc::as_ptr(&gc);

    drop(gc);
    collect_full();

    let dropped_ptr = get_dropped_ptr();
    assert!(
        !dropped_ptr.is_null(),
        "ZST: drop should be called - alloc={:?}, dropped={:?}",
        alloc_ptr.cast::<u8>(),
        dropped_ptr
    );
}

#[test]
fn test_back_to_back_allocations() {
    #[repr(C)]
    struct Small([u8; 8]);

    reset_dropped_ptr();

    for i in 0..10 {
        let gc = Gc::new(DropVerifier {
            _data: Small([0; 8]),
        });
        let ptr = Gc::as_ptr(&gc).cast::<u8>();
        drop(gc);
        collect_full();

        let dropped = get_dropped_ptr();
        assert_eq!(
            ptr, dropped,
            "Object {i}: drop called at wrong address - alloc={ptr:?}, dropped={dropped:?}",
        );
    }
}

#[test]
fn test_nested_struct_drop() {
    #[repr(C)]
    struct Inner {
        value: u64,
    }

    unsafe impl Trace for Inner {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    #[repr(C)]
    struct Outer {
        inner: DropVerifier<Inner>,
        extra: u64,
    }

    unsafe impl Trace for Outer {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    reset_dropped_ptr();

    let gc = Gc::new(Outer {
        inner: DropVerifier {
            _data: Inner { value: 123 },
        },
        extra: 456,
    });
    let alloc_ptr = Gc::as_ptr(&gc).cast::<u8>();

    drop(gc);
    collect_full();

    let dropped = get_dropped_ptr();
    assert_eq!(
        alloc_ptr, dropped,
        "Outer struct: drop called at wrong address - alloc={alloc_ptr:?}, dropped={dropped:?}",
    );
}

#[test]
fn test_array_drop() {
    #[repr(C)]
    struct Item {
        id: u32,
    }

    unsafe impl Trace for Item {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    #[repr(C)]
    struct ItemArray {
        items: [DropVerifier<Item>; 5],
        count: u32,
    }

    unsafe impl Trace for ItemArray {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    reset_dropped_ptr();

    let gc = Gc::new(ItemArray {
        items: [
            DropVerifier {
                _data: Item { id: 1 },
            },
            DropVerifier {
                _data: Item { id: 2 },
            },
            DropVerifier {
                _data: Item { id: 3 },
            },
            DropVerifier {
                _data: Item { id: 4 },
            },
            DropVerifier {
                _data: Item { id: 5 },
            },
        ],
        count: 5,
    });
    let alloc_ptr = Gc::as_ptr(&gc).cast::<u8>();

    drop(gc);
    collect_full();

    let dropped = get_dropped_ptr();
    assert!(
        alloc_ptr <= dropped && dropped < alloc_ptr.wrapping_add(128),
        "Array drop: dropped pointer should be in or near the allocated region - alloc={alloc_ptr:?}, dropped={dropped:?}",
    );
}

#[test]
fn test_32_byte_exact_alignment() {
    #[repr(align(32))]
    struct Align32Exact(#[allow(dead_code)] [u8; 32]);

    reset_dropped_ptr();

    let gc = Gc::new(DropVerifier {
        _data: Align32Exact([0xAB; 32]),
    });
    let alloc_ptr = Gc::as_ptr(&gc).cast::<u8>();

    assert_eq!(
        alloc_ptr.align_offset(32),
        0,
        "32-byte struct should be 32-byte aligned"
    );

    drop(gc);
    collect_full();

    let dropped = get_dropped_ptr();
    assert_eq!(
        alloc_ptr, dropped,
        "32-byte aligned struct: drop called at wrong address"
    );
}

#[test]
fn test_64_byte_exact_alignment() {
    #[repr(align(64))]
    struct Align64Exact(#[allow(dead_code)] [u8; 64]);

    reset_dropped_ptr();

    let gc = Gc::new(DropVerifier {
        _data: Align64Exact([0xCD; 64]),
    });
    let alloc_ptr = Gc::as_ptr(&gc).cast::<u8>();

    assert_eq!(
        alloc_ptr.align_offset(64),
        0,
        "64-byte struct should be 64-byte aligned"
    );

    drop(gc);
    collect_full();

    let dropped = get_dropped_ptr();
    assert_eq!(
        alloc_ptr, dropped,
        "64-byte aligned struct: drop called at wrong address"
    );
}
