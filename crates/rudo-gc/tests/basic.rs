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
        assert_eq!(**gc, i as i32);
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
    assert!(!Gc::is_dead(&x));
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
    let debug = format!("{:?}", x);
    assert!(debug.contains("42"));
    
    let display = format!("{}", x);
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
