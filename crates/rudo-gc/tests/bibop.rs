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
    // PAGE_SIZE = 4096, size class 16 = ~254 objects per page
    let objects: Vec<Gc<u64>> = (0..300).map(Gc::new).collect();

    // All should be accessible
    for (i, obj) in objects.iter().enumerate() {
        #[allow(clippy::cast_sign_loss)]
        let val = i as u64;
        assert_eq!(**obj, val);
    }
}
