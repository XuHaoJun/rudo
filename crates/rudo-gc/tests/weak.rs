//! Tests for the Weak<T> implementation.

use rudo_gc::{collect_full, Gc, Trace, Weak};
use std::cell::Cell;

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
// Basic Weak<T> tests
// ============================================================================

#[test]
fn test_weak_basic() {
    let gc = Gc::new(42);
    let weak = Gc::downgrade(&gc);

    // Weak should be alive
    assert!(weak.is_alive());

    // Weak should upgrade successfully
    let upgraded = weak.upgrade();
    assert!(upgraded.is_some());
    assert_eq!(*upgraded.unwrap(), 42);
}

#[test]
fn test_weak_counts() {
    let gc = Gc::new(123);

    // Initially no weak references
    assert_eq!(Gc::weak_count(&gc), 0);

    // Create weak references
    let weak1 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 1);

    let weak2 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 2);

    let weak3 = weak1.clone();
    assert_eq!(Gc::weak_count(&gc), 3);

    // Drop a weak reference
    drop(weak2);
    assert_eq!(Gc::weak_count(&gc), 2);

    // Verify counts through weak references
    assert_eq!(weak1.weak_count(), 2);
    assert_eq!(weak3.weak_count(), 2);
    assert_eq!(weak1.strong_count(), 1);
}

#[test]
fn test_weak_upgrade_after_drop() {
    let weak: Weak<i32>;

    {
        clear_roots!();
        let gc = Gc::new(999);
        root!(gc);
        weak = Gc::downgrade(&gc);

        // Should be alive while gc exists
        assert!(weak.is_alive());
        assert!(weak.upgrade().is_some());
    }

    // After gc is dropped & collected, upgrade should fail
    clear_roots!();
    collect_full();

    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());
}

#[test]
fn test_weak_strong_count_after_drop() {
    let weak: Weak<i32>;

    {
        clear_roots!();
        let gc = Gc::new(777);
        root!(gc);
        weak = Gc::downgrade(&gc);

        assert_eq!(weak.strong_count(), 1);
        assert_eq!(weak.weak_count(), 1);
        drop(gc);
    }

    clear_roots!();
    collect_full();

    // After collection, strong count should be 0
    assert_eq!(weak.strong_count(), 0);
}

#[test]
fn test_weak_ptr_eq() {
    let gc1 = Gc::new(1);
    let gc2 = Gc::new(2);

    let weak1_a = Gc::downgrade(&gc1);
    let weak1_b = Gc::downgrade(&gc1);
    let weak2 = Gc::downgrade(&gc2);

    assert!(Weak::ptr_eq(&weak1_a, &weak1_b));
    assert!(!Weak::ptr_eq(&weak1_a, &weak2));
}

#[test]
fn test_weak_default() {
    let weak: Weak<i32> = Weak::default();

    // Default weak reference should not be alive
    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());
    assert_eq!(weak.strong_count(), 0);
    assert_eq!(weak.weak_count(), 0);
}

#[test]
fn test_weak_clone() {
    let gc = Gc::new(42);
    let weak1 = Gc::downgrade(&gc);
    let weak2 = weak1.clone();

    assert!(Weak::ptr_eq(&weak1, &weak2));
    assert_eq!(Gc::weak_count(&gc), 2);

    drop(weak1);
    assert_eq!(Gc::weak_count(&gc), 1);

    assert!(weak2.is_alive());
}

#[test]
fn test_weak_debug() {
    let gc = Gc::new(42);
    let weak = Gc::downgrade(&gc);

    // Debug should not panic
    let debug_str = format!("{weak:?}");
    assert!(debug_str.contains("Weak"));
}

// ============================================================================
// Weak<T> with custom types
// ============================================================================

#[derive(Debug)]
#[allow(dead_code)]
struct DropTracker {
    id: i32,
    dropped: Cell<bool>,
}

#[allow(dead_code)]
impl DropTracker {
    const fn new(id: i32) -> Self {
        Self {
            id,
            dropped: Cell::new(false),
        }
    }

    fn is_dropped(&self) -> bool {
        self.dropped.get()
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

unsafe impl Trace for DropTracker {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

struct DroppableWrapper {
    marker: std::rc::Rc<Cell<bool>>,
}

impl Drop for DroppableWrapper {
    fn drop(&mut self) {
        self.marker.set(true);
    }
}

unsafe impl Trace for DroppableWrapper {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[test]
fn test_weak_value_correctly_dropped() {
    let dropped = std::rc::Rc::new(Cell::new(false));
    let dropped_clone = dropped.clone();

    let weak: Weak<DroppableWrapper>;

    {
        clear_roots!();
        let gc = Gc::new(DroppableWrapper {
            marker: dropped_clone,
        });
        root!(gc);
        weak = Gc::downgrade(&gc);

        // Value should not be dropped yet
        assert!(!dropped.get());
        drop(gc);
    }

    // Collect - value should be dropped
    clear_roots!();
    collect_full();

    // The value should be dropped, weak should not be alive
    assert!(dropped.get());
    assert!(!weak.is_alive());
    assert!(weak.upgrade().is_none());
}

// ============================================================================
// Weak<T> with collections
// ============================================================================

#[test]
fn test_multiple_weak_refs_same_object() {
    let gc = Gc::new(vec![1, 2, 3]);

    let weak_refs: Vec<Weak<Vec<i32>>> = (0..10).map(|_| Gc::downgrade(&gc)).collect();

    assert_eq!(Gc::weak_count(&gc), 10);

    // All should be alive
    for weak in &weak_refs {
        assert!(weak.is_alive());
    }

    // Drop half
    let remaining: Vec<_> = weak_refs.into_iter().skip(5).collect();
    assert_eq!(Gc::weak_count(&gc), 5);

    // Drop the original gc
    drop(gc);
    clear_roots!();
    collect_full();

    // All remaining weak refs should be dead
    for weak in &remaining {
        assert!(!weak.is_alive());
    }
    clear_roots!();
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn test_weak_upgrade_multiple_times() {
    let gc = Gc::new(String::from("hello"));
    let weak = Gc::downgrade(&gc);

    // Upgrade multiple times - should all succeed
    for _ in 0..5 {
        let upgraded = weak.upgrade();
        assert!(upgraded.is_some());
        // Let the upgraded Gc go out of scope
    }

    // Original gc still alive
    assert!(weak.is_alive());
}

#[test]
fn test_weak_ref_through_collection_cycle() {
    clear_roots!();
    let gc = Gc::new(42);
    root!(gc);
    let weak = Gc::downgrade(&gc);

    // Run multiple collections while gc is alive
    for _ in 0..5 {
        collect_full();
        assert!(weak.is_alive());
    }

    drop(gc);
    clear_roots!();
    collect_full();

    // Now it should be dead
    assert!(!weak.is_alive());
    clear_roots!();
}
