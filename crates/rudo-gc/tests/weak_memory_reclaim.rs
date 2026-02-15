//! Tests for Weak<T> memory reclamation.
//!
//! These tests verify that when all weak references are dropped,
//! the underlying memory is properly reclaimed during garbage collection.

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

#[derive(Trace)]
struct Inner {
    value: i32,
}

#[repr(C)]
struct LargeData {
    data: [u64; 1000],
}

unsafe impl Trace for LargeData {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

// ============================================================================
// Test 1: Memory reclaimed when all weak refs drop
// ============================================================================

#[test]
fn test_weak_memory_reclaimed_when_all_drop() {
    clear_roots!();

    let weak: Weak<Inner>;

    {
        let gc = Gc::new(Inner { value: 42 });
        root!(gc);
        weak = Gc::downgrade(&gc);
    }

    clear_roots!();
    collect_full();

    // Weak should be dead now
    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 2: Memory reclaimed with many weak refs
// ============================================================================

#[test]
fn test_weak_memory_reclaimed_many_refs() {
    clear_roots!();

    let weak_refs: Vec<Weak<Inner>>;
    let gc: Gc<Inner>;

    {
        gc = Gc::new(Inner { value: 123 });
        root!(gc);
        weak_refs = (0..100).map(|_| Gc::downgrade(&gc)).collect();
    }

    // Drop all weak refs
    drop(weak_refs);
    drop(gc);

    clear_roots!();
    collect_full();

    // Memory should be reclaimed - no panics
    clear_roots!();
}

// ============================================================================
// Test 3: Large object memory reclamation
// ============================================================================

#[test]
fn test_weak_large_object_memory_reclaimed() {
    clear_roots!();

    let weak: Weak<LargeData>;

    {
        let gc = Gc::new(LargeData { data: [0xAA; 1000] });
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

// ============================================================================
// Test 4: Partial weak refs keep memory
// ============================================================================

#[test]
fn test_weak_partial_refs_keep_memory() {
    clear_roots!();

    let gc = Gc::new(Inner { value: 42 });
    root!(gc);

    let weak1 = Gc::downgrade(&gc);
    let weak2 = Gc::downgrade(&gc);
    let weak3 = Gc::downgrade(&gc);

    // Drop only some weak refs
    drop(weak2);

    // Object should still be alive through weak1 and weak3
    assert!(weak1.upgrade().is_some());
    assert!(weak3.upgrade().is_some());

    // Drop remaining weak refs
    drop(weak1);
    drop(weak3);

    clear_roots!();
    collect_full();

    // Now memory should be reclaimed
    clear_roots!();
}

// ============================================================================
// Test 5: Weak with GcCell content reclaimed
// ============================================================================

#[test]
fn test_weak_gccell_content_reclaimed() {
    use rudo_gc::cell::GcCell;

    #[derive(Trace)]
    struct Node {
        value: GcCell<i32>,
    }

    clear_roots!();

    let weak: Weak<Node>;

    {
        let gc = Gc::new(Node {
            value: GcCell::new(100),
        });
        root!(gc);
        weak = Gc::downgrade(&gc);
    }

    clear_roots!();
    collect_full();

    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 6: Weak in Vec reclaimed
// ============================================================================

#[test]
fn test_weak_in_vec_reclaimed() {
    #[derive(Trace)]
    struct Item {
        value: i32,
    }

    clear_roots!();

    let weak_vec: Vec<Weak<Item>>;
    let gc_items: Vec<Gc<Item>>;

    {
        gc_items = (0..10).map(|i| Gc::new(Item { value: i })).collect();
        for gc in &gc_items {
            root!(gc);
        }
        weak_vec = gc_items.iter().map(Gc::downgrade).collect();
    }

    // All Gc items dropped, weak refs remain
    drop(gc_items);

    clear_roots!();
    collect_full();

    // All weak refs should be dead
    for weak in &weak_vec {
        assert!(!weak.is_alive());
        assert!(weak.upgrade().is_none());
    }

    clear_roots!();
}

// ============================================================================
// Test 7: Nested weak structure reclamation
// ============================================================================

#[test]
fn test_weak_nested_structure_reclaimed() {
    use rudo_gc::cell::GcCell;

    #[derive(Trace)]
    struct Outer {
        inner: GcCell<Option<Weak<Inner>>>,
        value: i32,
    }

    clear_roots!();

    let weak: Weak<Outer>;

    {
        let inner = Gc::new(Inner { value: 99 });
        root!(inner);

        let gc = Gc::new(Outer {
            inner: GcCell::new(Some(Gc::downgrade(&inner))),
            value: 42,
        });
        root!(gc);
        weak = Gc::downgrade(&gc);
    }

    clear_roots!();
    collect_full();

    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 8: Multiple cycles for complete reclamation
// ============================================================================

#[test]
fn test_weak_multiple_cycles_reclamation() {
    clear_roots!();

    let weak: Weak<Inner>;

    {
        let gc = Gc::new(Inner { value: 555 });
        root!(gc);
        weak = Gc::downgrade(&gc);
    }

    clear_roots!();

    // Run multiple collection cycles
    for _ in 0..3 {
        collect_full();
    }

    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 9: Weak with Rc inside Gc
// ============================================================================

#[test]
fn test_weak_with_rc_inside_gc() {
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Trace)]
    struct WithRc {
        rc: Rc<RefCell<i32>>,
    }

    clear_roots!();

    let weak: Weak<WithRc>;
    let rc_clone: Rc<RefCell<i32>>;

    {
        let rc = Rc::new(RefCell::new(42));
        rc_clone = rc.clone();

        let gc = Gc::new(WithRc { rc });
        root!(gc);
        weak = Gc::downgrade(&gc);
    }

    clear_roots!();
    collect_full();

    // Weak should be dead
    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());

    // Rc should have no strong refs from Gc now
    assert_eq!(Rc::strong_count(&rc_clone), 1); // Only our local clone

    clear_roots!();
    drop(rc_clone);
}

// ============================================================================
// Test 10: Weak count reaches zero
// ============================================================================

#[test]
fn test_weak_count_reaches_zero() {
    clear_roots!();

    let gc = Gc::new(Inner { value: 1 });
    root!(gc);

    assert_eq!(Gc::weak_count(&gc), 0);

    let weak1 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 1);

    let weak2 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 2);

    // Drop all weak refs
    drop(weak1);
    assert_eq!(Gc::weak_count(&gc), 1);

    drop(weak2);
    assert_eq!(Gc::weak_count(&gc), 0);

    // Re-register gc as root before collection
    root!(gc);
    collect_full();

    // Gc should still be alive (has root)
    assert!(Gc::downgrade(&gc).upgrade().is_some());

    clear_roots!();
}

// ============================================================================
// Test 11: Weak in HashMap reclaimed
// ============================================================================

#[test]
fn test_weak_in_hashmap_reclaimed() {
    use std::collections::HashMap;

    #[derive(Trace)]
    struct Value {
        data: i32,
    }

    clear_roots!();

    let weak_map: HashMap<i32, Weak<Value>>;
    let gc_map: HashMap<i32, Gc<Value>>;

    {
        let mut map = HashMap::new();
        for i in 0..5 {
            let gc = Gc::new(Value { data: i });
            root!(gc);
            map.insert(i, gc);
        }
        gc_map = map;
        weak_map = gc_map.iter().map(|(k, v)| (*k, Gc::downgrade(v))).collect();
    }

    drop(gc_map);

    clear_roots!();
    collect_full();

    // All weak refs should be dead
    for (k, weak) in &weak_map {
        assert!(!weak.is_alive(), "Key {k} should be dead");
    }

    clear_roots!();
}

// ============================================================================
// Test 13: Orphan weak ref reclamation
// ============================================================================

#[test]
fn test_weak_orphan_reclamation() {
    clear_roots!();

    // Create weak in a scope, let it become orphan
    let weak: Weak<Inner> = {
        let gc = Gc::new(Inner { value: 999 });
        Gc::downgrade(&gc)
    };

    // No roots, so gc should be collected
    clear_roots!();
    collect_full();

    // Weak should be dead
    assert!(!weak.is_alive());

    // Should be able to call upgrade without panic
    assert!(weak.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 14: Weak with drop flag set
// ============================================================================

#[test]
fn test_weak_drop_flag_set() {
    clear_roots!();

    let gc = Gc::new(Inner { value: 100 });
    root!(gc);
    let weak = Gc::downgrade(&gc);

    // Drop the Gc
    drop(gc);

    // Collect - should set DEAD_FLAG
    clear_roots!();
    collect_full();

    // Weak should be dead and have dead flag
    assert!(!weak.is_alive());
    let result = weak.upgrade();
    assert!(result.is_none());

    // Should not panic to check may_be_valid
    let _ = weak.may_be_valid();

    clear_roots!();
}

// ============================================================================
// Test 15: Memory reclaimed after weak and gc both drop
// ============================================================================

#[test]
fn test_weak_and_gc_both_drop() {
    clear_roots!();

    let weak: Weak<Inner>;

    {
        let gc = Gc::new(Inner { value: 200 });
        weak = Gc::downgrade(&gc);
        // gc goes out of scope here
    }

    // Both gc and weak are still alive (weak keeps memory mapped)
    // But when we collect, memory should be reclaimed

    clear_roots!();
    collect_full();

    // Weak should be dead
    assert!(!weak.is_alive());

    // Should be able to upgrade without panic
    assert!(weak.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Test 16: Verify no memory leak with repeated allocation
// ============================================================================

#[test]
fn test_weak_no_memory_leak_repeated_alloc() {
    clear_roots!();

    // Create and drop many Gc with weak refs
    for i in 0..50 {
        let gc = Gc::new(Inner { value: i });
        let weak = Gc::downgrade(&gc);

        drop(gc);
        drop(weak);
    }

    clear_roots!();
    collect_full();

    // Create one more to verify system still works
    let gc = Gc::new(Inner { value: 999 });
    let weak = Gc::downgrade(&gc);

    assert!(weak.upgrade().is_some());
    assert_eq!(weak.upgrade().unwrap().value, 999);

    clear_roots!();
}
