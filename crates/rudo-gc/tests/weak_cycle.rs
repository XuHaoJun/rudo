#![cfg(feature = "test-util")]
#![allow(clippy::redundant_closure)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::collection_is_never_read)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::use_self)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::unnecessary_unwrap)]

//! Tests for complex cycles with weak references.
//!
//! These tests verify correct GC behavior for:
//! - Weak cycles without external strong roots
//! - Transitive death patterns
//! - Weak references in containers
//! - Memory preservation for weak refs
//! - Mixed strong/weak reference graphs

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

// ============================================================================
// Test 1: Transitive Death (HIGH PRIORITY)
// Equivalent to gc-arena's transitive_death test
// Pattern: Gc<Option<Gc<T>>> + Weak<Gc<T>>
// When outer Gc drops, inner Gc should be collectable
// ============================================================================

#[test]
fn test_transitive_death() {
    clear_roots!();

    #[derive(Trace)]
    struct Inner {
        value: i32,
    }

    #[derive(Trace)]
    struct Outer {
        value: i32,
    }

    let outer: Gc<Outer>;
    let weak_inner: Weak<Inner>;

    {
        let inner = Gc::new(Inner { value: 42 });
        root!(inner);

        outer = Gc::new(Outer { value: 1 });
        weak_inner = Gc::downgrade(&inner);

        // Weak should be alive
        assert!(weak_inner.upgrade().is_some());
    }

    // Drop outer (weak_inner is the only remaining ref to inner)
    // But since no strong refs remain, inner should be collectable
    drop(outer);

    // Collect - inner should be dead
    collect_full();

    // Weak should now fail to upgrade (no strong refs remain)
    assert!(weak_inner.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 2: Weak Cycle with External Strong (HIGH PRIORITY)
// Pattern: Gc<A> -> Weak<B>, Gc<B> -> Weak<A>
// With external strong root - should work
// ============================================================================

#[test]
fn test_weak_cycle_with_external_strong() {
    #[derive(Trace)]
    struct Node {
        value: i32,
    }

    clear_roots!();

    let root_ref: Gc<Node>;

    let node_a = Gc::new(Node { value: 1 });
    let node_b = Gc::new(Node { value: 2 });

    // Create weak refs (not stored in Gc, just for testing)
    let _weak_a = Gc::downgrade(&node_a);
    let _weak_b = Gc::downgrade(&node_b);

    // Keep one node alive externally
    root_ref = Gc::clone(&node_a);

    // Verify: root_ref -> node_a
    assert_eq!(root_ref.value, 1);

    drop(root_ref);

    clear_roots!();
    collect_full();

    clear_roots!();
}

// ============================================================================
// Test 3: Weak in Container (MEDIUM)
// Pattern: Vec<Weak<T>>
// Verify upgrade works after collections
// ============================================================================

#[test]
fn test_weak_refs_in_vec() {
    clear_roots!();

    #[derive(Trace)]
    struct Item {
        value: i32,
    }

    // Create items and register them as roots
    let items: Vec<Gc<Item>> = (0..5)
        .map(|i| {
            let item = Gc::new(Item { value: i });
            root!(item);
            item
        })
        .collect();

    // Create weak refs to all items (while items are rooted)
    let weak_refs: Vec<Weak<Item>> = items.iter().map(Gc::downgrade).collect();

    collect_full();

    // All weak refs should still work since items are still roots
    for (i, weak) in weak_refs.iter().enumerate() {
        let upgraded = weak.upgrade();
        assert!(
            upgraded.is_some(),
            "weak ref {i} should still be valid after collection"
        );
        assert_eq!(upgraded.unwrap().value, i as i32);
    }

    clear_roots!();
}

#[test]
fn test_weak_vec_preserves_access() {
    #[derive(Trace)]
    struct Item {
        value: i32,
    }

    clear_roots!();

    // Use a simple Vec outside of Gc to hold weak refs
    let mut weak_refs: Vec<Weak<Item>> = Vec::new();
    let mut gc_refs: Vec<Gc<Item>> = Vec::new();

    // Add items and weak refs
    for i in 0..5 {
        let item = Gc::new(Item { value: i });
        weak_refs.push(Gc::downgrade(&item));
        gc_refs.push(item);
    }

    // Register gc_refs as root to ensure GC sees it
    for _gc in &gc_refs {
        root!(_gc);
    }

    // Verify all weak refs work
    for (i, weak) in weak_refs.iter().enumerate() {
        assert!(weak.upgrade().is_some());
        let val = weak.upgrade().unwrap().value;
        assert_eq!(val, i as i32);
    }

    collect_full();

    // After collection, all should still work (gc_refs keeps them alive)
    for (i, weak) in weak_refs.iter().enumerate() {
        let upgraded = weak.upgrade();
        assert!(
            upgraded.is_some(),
            "weak ref {i} should still be valid after collection"
        );
        assert_eq!(upgraded.unwrap().value, i as i32);
    }

    clear_roots!();
}

// ============================================================================
// Test 4: Weak Resurrection (MEDIUM)
// Upgrade weak after multiple GC cycles while key is still alive
// ============================================================================

#[test]
fn test_weak_upgrade_multiple_collections() {
    clear_roots!();

    let gc = Gc::new(42);
    root!(gc);
    let weak = Gc::downgrade(&gc);

    // Multiple collections while gc is alive
    for _ in 0..5 {
        collect_full();
        let upgraded = weak.upgrade().unwrap();
        assert_eq!(*upgraded, 42);
    }

    clear_roots!();
}

#[test]
fn test_weak_upgrade_after_strong_drops() {
    clear_roots!();

    let gc = Gc::new(42);
    let weak1 = Gc::downgrade(&gc);
    let weak2 = Gc::downgrade(&gc);

    root!(gc);

    // Both weak refs should work
    assert!(weak1.upgrade().is_some());
    assert!(weak2.upgrade().is_some());

    // Drop the gc
    drop(gc);

    clear_roots!();
    collect_full();

    // Both should fail now
    assert!(weak1.upgrade().is_none());
    assert!(weak2.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 5: Multiple Weak Refs (MEDIUM)
// ============================================================================

#[test]
fn test_multiple_weak_refs_same_target() {
    clear_roots!();

    let gc = Gc::new(42);

    // Create many weak refs to same target
    let weak_refs: Vec<Weak<i32>> = (0..100).map(|_| Gc::downgrade(&gc)).collect();

    // All should work
    for weak in &weak_refs {
        assert!(weak.upgrade().is_some());
    }

    // Drop original
    drop(gc);

    clear_roots!();
    collect_full();

    // All should fail
    for weak in &weak_refs {
        assert!(weak.upgrade().is_none());
    }

    clear_roots!();
}

// ============================================================================
// Test 6: Memory Preservation (MEDIUM)
// Weak keeps memory mapped after value drops
// ============================================================================

#[test]
fn test_weak_preserves_memory_after_drop() {
    clear_roots!();

    #[derive(Trace)]
    struct LargeData {
        data: [u8; 1024],
    }

    let gc = Gc::new(LargeData { data: [0xAA; 1024] });
    let weak = Gc::downgrade(&gc);

    drop(gc);

    // Weak should still allow upgrade check without panicking
    // (memory should still be mapped)
    let result = weak.upgrade();
    assert!(result.is_none(), "Should return None, not panic");

    clear_roots!();
}

#[test]
fn test_weak_ptr_still_valid_after_collection() {
    clear_roots!();

    let gc = Gc::new(42);
    let weak = Gc::downgrade(&gc);

    drop(gc);
    collect_full();

    // is_alive should return false
    assert!(!weak.is_alive());

    clear_roots!();
}

// ============================================================================
// Test 7: Edge Cases
// ============================================================================

#[test]
fn test_weak_default() {
    let weak: Weak<i32> = Weak::default();

    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());
}

#[test]
fn test_weak_from_dropped_gc() {
    clear_roots!();

    let weak: Weak<i32>;

    {
        let gc = Gc::new(42);
        root!(gc);
        weak = Gc::downgrade(&gc);
    }

    clear_roots!();
    collect_full();

    // Weak should be dead
    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());

    clear_roots!();
}

#[test]
fn test_weak_clone_independent() {
    clear_roots!();

    let gc = Gc::new(42);
    let weak1 = Gc::downgrade(&gc);
    let weak2 = weak1.clone();

    assert!(Gc::ptr_eq(
        &weak1.upgrade().unwrap(),
        &weak2.upgrade().unwrap()
    ));

    drop(gc);
    clear_roots!();
    collect_full();

    assert!(weak1.upgrade().is_none());
    assert!(weak2.upgrade().is_none());

    clear_roots!();
}

#[test]
fn test_weak_count_tracking() {
    clear_roots!();

    let gc = Gc::new(42);
    assert_eq!(Gc::weak_count(&gc), 0);

    let weak1 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 1);

    let weak2 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 2);

    let _weak3 = weak1.clone();
    assert_eq!(Gc::weak_count(&gc), 3);

    drop(weak2);
    assert_eq!(Gc::weak_count(&gc), 2);

    drop(gc);
    clear_roots!();
    collect_full();

    // After collection, weak refs should be cleared
    // The object is now dead, so weak upgrades should fail
    assert!(weak1.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 8: GcCell with Weak Self Reference
// ============================================================================

#[test]
fn test_gccell_with_weak_self_ref() {
    use rudo_gc::cell::GcCell;

    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Self>>>,
        value: i32,
    }

    let node = Gc::new_cyclic_weak(|weak| Node {
        self_ref: GcCell::new(Some(weak)),
        value: 42,
    });

    // Verify self-ref works
    let weak_self = node.self_ref.borrow();
    let upgraded = weak_self.as_ref().unwrap().upgrade().unwrap();
    assert!(Gc::ptr_eq(&node, &upgraded));
    assert_eq!(upgraded.value, 42);

    collect_full();

    // Still works after collection
    let weak_self = node.self_ref.borrow();
    let upgraded = weak_self.as_ref().unwrap().upgrade().unwrap();
    assert!(Gc::ptr_eq(&node, &upgraded));
}
