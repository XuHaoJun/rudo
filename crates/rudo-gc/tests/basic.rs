//! Basic allocation and collection tests for rudo-gc.

use rudo_gc::{collect, Gc, Trace};

#[test]
fn test_basic_allocation() {
    let x = Gc::new(42);
    assert_eq!(*x, 42);
}

#[test]
fn test_deref() {
    let x = Gc::new(String::from("hello"));
    assert_eq!(&*x, "hello");
    assert_eq!(x.len(), 5);
}

#[test]
fn test_clone() {
    let x = Gc::new(42);
    let y = Gc::clone(&x);
    assert_eq!(*x, 42);
    assert_eq!(*y, 42);
    assert!(Gc::ptr_eq(&x, &y));
}

#[test]
fn test_ref_count() {
    let x = Gc::new(42);
    assert_eq!(Gc::ref_count(&x).get(), 1);

    let y = Gc::clone(&x);
    assert_eq!(Gc::ref_count(&x).get(), 2);
    assert_eq!(Gc::ref_count(&y).get(), 2);

    drop(y);
    assert_eq!(Gc::ref_count(&x).get(), 1);
}

#[test]
fn test_drop_and_collect() {
    let x = Gc::new(42);
    drop(x);
    collect(); // Should not panic
}

#[test]
fn test_multiple_allocations() {
    let values: Vec<Gc<i32>> = (0..100).map(Gc::new).collect();
    for (i, gc) in values.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let i_i32 = i as i32;
        assert_eq!(**gc, i_i32);
    }
}

#[test]
fn test_different_size_classes() {
    // Small (16 bytes)
    let small = Gc::new(42u64);
    assert_eq!(*small, 42);

    // Medium (32 bytes)
    let medium = Gc::new([1u64, 2, 3, 4]);
    assert_eq!(medium[0], 1);

    // Larger (64 bytes)
    let large = Gc::new([0u64; 8]);
    assert_eq!(large.len(), 8);
}

#[test]
fn test_ptr_eq() {
    let x = Gc::new(42);
    let y = Gc::clone(&x);
    let z = Gc::new(42);

    assert!(Gc::ptr_eq(&x, &y));
    assert!(!Gc::ptr_eq(&x, &z));
}

#[test]
fn test_is_dead() {
    let x = Gc::new(42);
    assert!(!Gc::is_dead_or_unrooted(&x));
}

#[test]
fn test_try_deref() {
    let x = Gc::new(42);
    assert_eq!(Gc::try_deref(&x), Some(&42));
}

#[test]
fn test_try_clone() {
    let x = Gc::new(42);
    let y = Gc::try_clone(&x).unwrap();
    assert!(Gc::ptr_eq(&x, &y));
}

#[test]
fn test_debug_display() {
    let x = Gc::new(42);
    let debug = format!("{x:?}");
    assert!(debug.contains("42"));

    let display = format!("{x}");
    assert_eq!(display, "42");
}

#[test]
fn test_partial_eq() {
    let x = Gc::new(42);
    let y = Gc::new(42);
    let z = Gc::new(100);

    assert_eq!(x, y);
    assert_ne!(x, z);
}

#[test]
fn test_default() {
    let x: Gc<i32> = Gc::default();
    assert_eq!(*x, 0);
}

#[test]
fn test_from() {
    let x: Gc<i32> = Gc::from(42);
    assert_eq!(*x, 42);
}

// ============================================================================
// T065/T066: Zero-Sized Type (ZST) tests
// ============================================================================

#[test]
fn test_zst_unit() {
    // Unit type is a ZST
    let x = Gc::new(());
    let y = Gc::new(());

    // Both should work
    assert_eq!(*x, ());
    assert_eq!(*y, ());
}

/// A custom ZST.
#[derive(Debug, Clone, Copy, PartialEq, Trace)]
struct EmptyStruct;

#[test]
fn test_zst_custom_struct() {
    let x = Gc::new(EmptyStruct);
    let y = Gc::new(EmptyStruct);

    assert_eq!(*x, EmptyStruct);
    assert_eq!(*y, EmptyStruct);
}

#[test]
fn test_zst_clone() {
    let x = Gc::new(());
    let y = Gc::clone(&x);

    assert_eq!(*x, ());
    assert_eq!(*y, ());
    // ZST clones should point to same allocation
    assert!(Gc::ptr_eq(&x, &y));
}

#[test]
fn test_zst_drop_collect() {
    {
        let _x = Gc::new(());
        let _y = Gc::new(());
        let _z = Gc::new(());
    }
    // Should not panic
    collect();
}

#[test]
fn test_zst_many_allocations() {
    // Allocate many ZSTs - should be efficient
    let units: Vec<Gc<()>> = (0..1000).map(|_| Gc::new(())).collect();

    for unit in &units {
        assert_eq!(**unit, ());
    }
}
