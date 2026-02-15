//! Tests for Weak<T>`::cast()` pointer casting functionality.
//!
//! These tests verify that `Weak::cast()` works correctly for casting
//! weak pointers between compatible types.

use rudo_gc::{collect_full, Gc, Trace, Weak};

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

#[derive(Trace, Debug)]
struct Inner {
    value: i32,
}

#[derive(Trace, Debug)]
struct Outer {
    inner: Inner,
}

// ============================================================================
// Test 1: Basic cast between compatible types
// ============================================================================

#[test]
fn test_weak_cast_basic() {
    let gc = Gc::new(Inner { value: 42 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    let weak_cast: Weak<u8> = weak.cast::<u8>();

    assert!(weak_cast.is_alive());
    assert!(weak_cast.may_be_valid());
}

#[test]
fn test_weak_cast_upgrade_after_cast() {
    let gc = Gc::new(Inner { value: 42 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    let weak_cast: Weak<u8> = weak.cast::<u8>();
    let upgraded = weak_cast.upgrade();

    assert!(upgraded.is_some());
}

// ============================================================================
// Test 2: Cast with larger type
// ============================================================================

#[test]
fn test_weak_cast_to_larger_type() {
    let gc = Gc::new(Inner { value: 42 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    // Cast to a type with same size (i32 -> u32)
    let weak_cast: Weak<u32> = weak.cast::<u32>();

    assert!(weak_cast.is_alive());
    assert!(weak_cast.upgrade().is_some());
}

// ============================================================================
// Test 3: Cast with struct types
// ============================================================================

#[test]
fn test_weak_cast_struct_to_u8() {
    let gc = Gc::new(Inner { value: 123 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    let weak_bytes: Weak<u8> = weak.cast::<u8>();

    assert!(weak_bytes.is_alive());
    let upgraded = weak_bytes.upgrade();
    assert!(upgraded.is_some());
}

#[test]
fn test_weak_cast_nested_struct() {
    let gc = Gc::new(Outer {
        inner: Inner { value: 99 },
    });
    let weak: Weak<Outer> = Gc::downgrade(&gc);

    let weak_inner: Weak<Inner> = weak.cast::<Inner>();

    let upgraded = weak_inner.upgrade();
    assert!(upgraded.is_some());
    assert_eq!(upgraded.unwrap().value, 99);
}

// ============================================================================
// Test 4: Cast with null weak
// ============================================================================

#[test]
fn test_weak_cast_null() {
    let weak: Weak<Inner> = Weak::default();

    let weak_cast: Weak<u8> = weak.cast::<u8>();

    assert!(!weak_cast.is_alive());
    assert!(!weak_cast.may_be_valid());
    assert!(weak_cast.upgrade().is_none());
}

// ============================================================================
// Test 5: Cast after collection
// ============================================================================

#[test]
fn test_weak_cast_after_collection() {
    let weak: Weak<Inner>;

    {
        clear_roots!();
        let gc = Gc::new(Inner { value: 42 });
        root!(gc);
        weak = Gc::downgrade(&gc);
    }

    clear_roots!();
    collect_full();

    let weak_cast: Weak<u8> = weak.cast::<u8>();

    assert!(!weak_cast.is_alive());
    assert!(weak_cast.upgrade().is_none());
}

// ============================================================================
// Test 6: Multiple casts
// ============================================================================

#[test]
fn test_weak_cast_multiple_times() {
    let gc = Gc::new(Inner { value: 42 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    // Cast multiple times through different types
    let weak_u8: Weak<u8> = weak.cast::<u8>();
    let weak_u16: Weak<u16> = weak_u8.cast::<u16>();
    let weak_u32: Weak<u32> = weak_u16.cast::<u32>();
    let weak_converted: Weak<i32> = weak_u32.cast::<i32>();

    assert!(weak_converted.is_alive());
    assert!(weak_converted.upgrade().is_some());
}

// ============================================================================
// Test 7: Cast with clone
// ============================================================================

#[test]
fn test_weak_cast_with_clone() {
    let gc = Gc::new(Inner { value: 55 });
    let weak1: Weak<Inner> = Gc::downgrade(&gc);
    let weak2 = weak1.clone();

    let weak1_cast: Weak<u8> = weak1.cast::<u8>();
    let weak2_cast: Weak<u8> = weak2.cast::<u8>();

    assert!(weak1_cast.is_alive());
    assert!(weak2_cast.is_alive());
    assert!(Weak::ptr_eq(&weak1_cast, &weak2_cast));
}

// ============================================================================
// Test 8: Cast preserves weak count
// ============================================================================

#[test]
fn test_weak_cast_preserves_count() {
    let gc = Gc::new(Inner { value: 42 });
    let weak1: Weak<Inner> = Gc::downgrade(&gc);
    let weak2 = weak1.clone();

    assert_eq!(Gc::weak_count(&gc), 2);

    let weak1_cast: Weak<u8> = weak1.cast::<u8>();
    let weak2_cast: Weak<u8> = weak2.cast::<u8>();

    // Count should remain the same (same underlying allocation)
    assert_eq!(Gc::weak_count(&gc), 2);
    assert_eq!(weak1_cast.weak_count(), 2);
    assert_eq!(weak2_cast.weak_count(), 2);
}

// ============================================================================
// Test 9: Cast with GcCell containing Weak
// ============================================================================

#[test]
fn test_weak_cast_with_gccell() {
    use rudo_gc::cell::GcCell;

    #[derive(Trace)]
    struct Node {
        weak_ref: GcCell<Option<Weak<Self>>>,
        value: i32,
    }

    let node = Gc::new_cyclic_weak(|weak| Node {
        weak_ref: GcCell::new(Some(weak)),
        value: 100,
    });

    // Get weak ref and cast it (need to clone since cast takes ownership)
    let weak_original = node.weak_ref.borrow();
    let weak_inner = weak_original.as_ref().unwrap();

    // Cast the weak ref (clone first since cast consumes self)
    let weak_cast: Weak<u8> = weak_inner.clone().cast::<u8>();

    // Should still be able to upgrade to get the original
    // (through the uncasted weak ref)
    let upgraded_original = weak_inner.upgrade();
    assert!(upgraded_original.is_some());
    assert_eq!(upgraded_original.unwrap().value, 100);

    // The casted weak should still be alive
    assert!(weak_cast.is_alive());
}

// ============================================================================
// Test 10: Cast with complex nested data
// ============================================================================

#[test]
fn test_weak_cast_complex_nested() {
    #[derive(Trace)]
    struct Complex {
        a: i32,
        b: i64,
        c: i32,
    }

    let gc = Gc::new(Complex { a: 1, b: 2, c: 3 });
    let weak: Weak<Complex> = Gc::downgrade(&gc);

    // Cast through different type layouts
    let weak_i64: Weak<i64> = weak.cast::<i64>();

    assert!(weak_i64.is_alive());
    assert!(weak_i64.upgrade().is_some());
}

// ============================================================================
// Test 11: Cast and upgrade with value check
// ============================================================================

#[test]
fn test_weak_cast_upgrade_value_integrity() {
    let gc = Gc::new(Inner { value: 999 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    // Cast and then upgrade (need to clone since cast consumes self)
    let weak_cast: Weak<u8> = weak.clone().cast::<u8>();
    let upgraded = weak_cast.upgrade();

    // Note: The upgraded value will be garbage since we cast Inner to u8
    // This is undefined behavior but shouldn't crash in release mode
    // The test just verifies we can perform the operations without panic
    assert!(upgraded.is_some());

    // Verify original weak still works
    let original_upgraded = weak.upgrade();
    assert!(original_upgraded.is_some());
    assert_eq!(original_upgraded.unwrap().value, 999);
}

// ============================================================================
// Test 12: Cast with may_be_valid
// ============================================================================

#[test]
fn test_weak_cast_may_be_valid() {
    let gc = Gc::new(Inner { value: 42 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    let weak_cast: Weak<u8> = weak.clone().cast::<u8>();

    assert!(weak.may_be_valid());
    assert!(weak_cast.may_be_valid());
}

// ============================================================================
// Test 13: Cast with strong count
// ============================================================================

#[test]
fn test_weak_cast_strong_count() {
    let gc = Gc::new(Inner { value: 42 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    let weak_cast: Weak<u8> = weak.clone().cast::<u8>();

    assert_eq!(weak.strong_count(), 1);
    assert_eq!(weak_cast.strong_count(), 1);
}

// ============================================================================
// Test 14: Drop original after cast
// ============================================================================

#[test]
fn test_weak_cast_after_original_dropped() {
    let gc = Gc::new(Inner { value: 42 });
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    // Cast before dropping (need to clone since cast consumes self)
    let weak_cast: Weak<u8> = weak.clone().cast::<u8>();

    // Drop original
    drop(gc);
    drop(weak);

    // Casted weak should still work if value is alive (via other refs)
    // But since we dropped the only strong ref, value should be dead
    clear_roots!();
    collect_full();

    assert!(!weak_cast.is_alive());
    assert!(weak_cast.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 15: Cast with array type
// ============================================================================

#[test]
fn test_weak_cast_array() {
    #[derive(Trace)]
    struct ArrayWrapper {
        data: [u8; 16],
    }

    let gc = Gc::new(ArrayWrapper { data: [0u8; 16] });
    let weak: Weak<ArrayWrapper> = Gc::downgrade(&gc);

    // Cast to array of different size
    let weak_array: Weak<[u8; 8]> = weak.cast::<[u8; 8]>();

    assert!(weak_array.is_alive());
    assert!(weak_array.upgrade().is_some());
}
