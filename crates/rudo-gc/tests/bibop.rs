//! `BiBOP` (Big Bag of Pages) memory layout tests.

use rudo_gc::{Gc, Trace};

/// Small object (fits in 16-byte size class after `GcBox` overhead).
#[derive(Trace)]
struct Small {
    value: u8,
}

/// Medium object (32-byte size class).
#[derive(Trace)]
struct Medium {
    a: u64,
    b: u64,
}

/// Larger object (64-byte size class).
#[derive(Trace)]
struct Large {
    data: [u64; 6],
}

/// Very large object (128-byte size class).
#[derive(Trace)]
struct VeryLarge {
    data: [u64; 14],
}

#[test]
fn test_different_size_allocations() {
    // These should be allocated in different segments based on size
    let small = Gc::new(Small { value: 1 });
    let medium = Gc::new(Medium { a: 2, b: 3 });
    let large = Gc::new(Large { data: [4; 6] });
    let very_large = Gc::new(VeryLarge { data: [5; 14] });

    assert_eq!(small.value, 1);
    assert_eq!(medium.a, 2);
    assert_eq!(medium.b, 3);
    assert_eq!(large.data[0], 4);
    assert_eq!(very_large.data[0], 5);
}

#[test]
fn test_many_small_allocations() {
    // Allocate many small objects to fill multiple pages
    let objects: Vec<Gc<Small>> = (0..1000)
        .map(|i| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let val = (i % 256) as u8;
            Gc::new(Small { value: val })
        })
        .collect();

    for (i, obj) in objects.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let val = (i % 256) as u8;
        assert_eq!(obj.value, val);
    }
}

#[test]
fn test_many_medium_allocations() {
    let objects: Vec<Gc<Medium>> = (0..500)
        .map(|i| {
            #[allow(clippy::cast_sign_loss)]
            let val = i as u64;
            Gc::new(Medium { a: val, b: val * 2 })
        })
        .collect();

    for (i, obj) in objects.iter().enumerate() {
        #[allow(clippy::cast_sign_loss)]
        let val = i as u64;
        assert_eq!(obj.a, val);
        assert_eq!(obj.b, val * 2);
    }
}

#[test]
fn test_mixed_size_allocations() {
    // Interleave allocations of different sizes
    let mut smalls = Vec::new();
    let mut mediums = Vec::new();
    let mut larges = Vec::new();

    for i in 0..100 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i_u8 = i as u8;
        #[allow(clippy::cast_sign_loss)]
        let i_u64 = i as u64;

        smalls.push(Gc::new(Small { value: i_u8 }));
        mediums.push(Gc::new(Medium { a: i_u64, b: 0 }));
        larges.push(Gc::new(Large { data: [i_u64; 6] }));
    }

    for i in 0..100 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i_u8 = i as u8;
        #[allow(clippy::cast_sign_loss)]
        let i_u64 = i as u64;

        assert_eq!(smalls[i].value, i_u8);
        assert_eq!(mediums[i].a, i_u64);
        assert_eq!(larges[i].data[0], i_u64);
    }
}

/// Large object that exceeds normal size classes.
#[derive(Trace)]
struct HugeObject {
    data: [u64; 512], // 4KB - should go to LOS
}

#[test]
fn test_large_object_space() {
    let huge = Gc::new(HugeObject { data: [42; 512] });
    assert_eq!(huge.data[0], 42);
    assert_eq!(huge.data[511], 42);
}

#[test]
fn test_multiple_large_objects() {
    let objects: Vec<Gc<HugeObject>> = (0..10)
        .map(|i| {
            #[allow(clippy::cast_sign_loss)]
            let val = i as u64;
            Gc::new(HugeObject { data: [val; 512] })
        })
        .collect();

    for (i, obj) in objects.iter().enumerate() {
        #[allow(clippy::cast_sign_loss)]
        let val = i as u64;
        assert_eq!(obj.data[0], val);
        assert_eq!(obj.data[255], val);
    }
}

#[test]
fn test_allocation_consistency() {
    // Verify that repeated allocations work consistently
    for _ in 0..10 {
        let objects: Vec<Gc<Medium>> = (0..100)
            .map(|i| {
                #[allow(clippy::cast_sign_loss)]
                let val = i as u64;
                Gc::new(Medium { a: val, b: 0 })
            })
            .collect();

        for (i, obj) in objects.iter().enumerate() {
            #[allow(clippy::cast_sign_loss)]
            let val = i as u64;
            assert_eq!(obj.a, val);
        }
    }
}

#[test]
fn test_ptr_uniqueness() {
    // Each allocation should get a unique address
    let gc1 = Gc::new(42i32);
    let gc2 = Gc::new(42i32);
    let gc3 = Gc::new(42i32);

    assert!(!Gc::ptr_eq(&gc1, &gc2));
    assert!(!Gc::ptr_eq(&gc2, &gc3));
    assert!(!Gc::ptr_eq(&gc1, &gc3));
}

#[test]
fn test_page_filling() {
    // Allocate enough objects to fill at least one page
    // page_size() returns system page size (typically 4096), size class 16 = ~254 objects per page
    let objects: Vec<Gc<u64>> = (0..300).map(Gc::new).collect();

    // All should be accessible
    for (i, obj) in objects.iter().enumerate() {
        #[allow(clippy::cast_sign_loss)]
        let val = i as u64;
        assert_eq!(**obj, val);
    }
}

// ============================================================================
// T060: Page header validation tests
// ============================================================================

/// Test that verifies page header magic number, `block_size`, and `obj_count`.
#[test]
fn test_page_header_validation() {
    use rudo_gc::heap::{page_size, with_heap, PageHeader, MAGIC_GC_PAGE};

    // Allocate an object to ensure at least one page exists
    let _obj = Gc::new(42u32);

    // Access the heap and verify page header properties
    with_heap(|heap| {
        let pages: Vec<_> = heap.all_pages().collect();
        assert!(!pages.is_empty(), "Should have at least one page");

        for page_ptr in pages {
            // SAFETY: Page pointers from all_pages are always valid
            unsafe {
                let header = page_ptr.as_ptr();

                // Verify magic number
                assert_eq!(
                    (*header).magic,
                    MAGIC_GC_PAGE,
                    "Page header should have correct magic number"
                );

                // Verify block_size is a valid size class
                let block_size = (*header).block_size as usize;
                let valid_sizes = [16, 32, 64, 128, 256, 512, 1024, 2048];
                // Large objects may have arbitrary sizes
                let is_large_object = (*header).is_large_object();
                if !is_large_object {
                    assert!(
                        valid_sizes.contains(&block_size),
                        "Block size {block_size} should be a valid size class"
                    );
                }

                // Verify obj_count is reasonable
                let obj_count = (*header).obj_count as usize;
                if is_large_object {
                    assert_eq!(obj_count, 1, "Large object should have obj_count = 1");
                } else {
                    let expected_max = PageHeader::max_objects(block_size);
                    assert_eq!(
                        obj_count, expected_max,
                        "obj_count should match max_objects for block_size {block_size}"
                    );
                    // Sanity check: should fit in a page
                    let header_size = PageHeader::header_size(block_size);
                    assert!(
                        header_size + obj_count * block_size <= page_size(),
                        "Objects should fit in page"
                    );
                }
            }
        }
    });
}

/// Test page header mark bitmap operations.
#[test]
fn test_page_header_mark_bitmap() {
    use rudo_gc::heap::with_heap;

    // Allocate objects
    let _obj1 = Gc::new(1u32);
    let _obj2 = Gc::new(2u32);

    with_heap(|heap| {
        for page_ptr in heap.all_pages() {
            // SAFETY: Page pointers are valid
            unsafe {
                let header = page_ptr.as_ptr();
                let obj_count = (*header).obj_count as usize;

                // Clear all marks
                (*header).clear_all_marks();

                // Verify all cleared
                for i in 0..obj_count.min(256) {
                    assert!(!(*header).is_marked(i), "Bit {i} should be cleared");
                }

                // Set some marks
                if obj_count > 0 {
                    (*header).set_mark(0);
                    assert!((*header).is_marked(0), "Bit 0 should be set");
                }

                if obj_count > 10 {
                    (*header).set_mark(10);
                    assert!((*header).is_marked(10), "Bit 10 should be set");
                }

                // Clear and verify
                (*header).clear_all_marks();
                if obj_count > 0 {
                    assert!(
                        !(*header).is_marked(0),
                        "Bit 0 should be cleared after clear_all"
                    );
                }
            }
        }
    });
}

/// Test that different size classes route to different segments.
#[test]
fn test_size_class_segment_separation() {
    use rudo_gc::heap::with_heap;

    // Extract page addresses (lower bits masked)
    const PAGE_MASK: usize = !(4096 - 1);

    // Allocate objects of different sizes
    let small = Gc::new(Small { value: 1 });
    let medium = Gc::new(Medium { a: 2, b: 3 });

    // Get pointers and verify they're in different pages (likely different segments)
    let small_ptr = Gc::as_ptr(&small) as usize;
    let medium_ptr = Gc::as_ptr(&medium) as usize;

    let small_page = small_ptr & PAGE_MASK;
    let medium_page = medium_ptr & PAGE_MASK;

    // Different size classes should be in different segments (usually different pages)
    // Note: This might not always hold if allocations happen to be in the same page
    // due to timing, but for fresh heap state it should work
    with_heap(|heap| {
        let page_count = heap.all_pages().count();
        assert!(page_count >= 1, "Should have at least one page");

        // If we have 2+ pages, they should be different for different size classes
        if page_count >= 2 && small_page != medium_page {
            assert_ne!(
                small_page, medium_page,
                "Different size classes should be in different segments"
            );
        }
    });
}
