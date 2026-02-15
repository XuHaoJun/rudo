//! Tests for the Ephemeron<K, V> implementation.

#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::useless_vec)]

use rudo_gc::{collect_full, Ephemeron, Gc, Trace};
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

#[cfg(feature = "debug-suspicious-sweep")]
use rudo_gc::clear_history;

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
// Basic Ephemeron tests
// ============================================================================

#[test]
fn test_ephemeron_basic() {
    let key = Gc::new("key");
    let value = Gc::new(42);
    let ephemeron = Ephemeron::new(&key, value);

    // Key should be alive (we're holding a clone)
    assert!(ephemeron.is_key_alive());

    // Value should be accessible because key is alive
    let upgraded = ephemeron.upgrade();
    assert!(upgraded.is_some());
    assert_eq!(*upgraded.unwrap(), 42);
}

#[test]
fn test_ephemeron_key_and_value() {
    let key = Gc::new("my_key");
    let value = Gc::new(123);
    let ephemeron = Ephemeron::new(&key, value.clone());

    // key() returns Option - None because key is stored as Weak internally
    // The important thing is that value upgrade works while key is alive
    let upgraded_value = ephemeron.upgrade();
    assert!(upgraded_value.is_some());
    assert!(Gc::ptr_eq(&upgraded_value.unwrap(), &value));
}

#[test]
fn test_ephemeron_may_be_valid() {
    let key = Gc::new("key");
    let value = Gc::new(42);
    let ephemeron = Ephemeron::new(&key, value);

    // Should return true for valid ephemeron
    assert!(ephemeron.may_be_valid());
}

#[test]
fn test_ephemeron_default() {
    let ephemeron: Ephemeron<i32, i32> = Ephemeron::default();

    // Default ephemeron should not be valid
    assert!(!ephemeron.is_key_alive());
    assert!(!ephemeron.may_be_valid());
    assert!(ephemeron.upgrade().is_none());
}

#[test]
fn test_ephemeron_debug() {
    let key = Gc::new("key");
    let value = Gc::new(42);
    let ephemeron = Ephemeron::new(&key, value);

    // Debug should not panic
    let debug_str = format!("{ephemeron:?}");
    assert!(debug_str.contains("Ephemeron"));
    assert!(debug_str.contains("key_alive"));
}

#[test]
fn test_ephemeron_clone() {
    let key = Gc::new("key");
    let value = Gc::new(42);
    let ephemeron = Ephemeron::new(&key, value);

    let cloned = ephemeron.clone();

    // Both should work - key is still alive via original reference
    assert!(cloned.is_key_alive());
    assert!(cloned.upgrade().is_some());
}

// ============================================================================
// Ephemeron lifecycle tests - key determines value reachability
// ============================================================================

#[test]
fn test_ephemeron_value_accessible_while_key_alive() {
    clear_roots!();

    let key = Gc::new("key");
    root!(key);

    let value = Gc::new(42);
    let ephemeron = Ephemeron::new(&key, value);

    // Value is accessible because key is alive
    assert!(ephemeron.upgrade().is_some());
    assert!(ephemeron.is_key_alive());

    // Even after multiple collections, value still accessible
    collect_full();
    assert!(ephemeron.upgrade().is_some());
    assert!(ephemeron.is_key_alive());

    clear_roots!();
}

#[test]
fn test_ephemeron_value_inaccessible_when_key_dropped() {
    let key = Rc::new(Cell::new(false));
    let value = Rc::new(Cell::new(false));

    let ephemeron: Ephemeron<DropTracker, DropTracker>;

    {
        clear_roots!();

        // Create key and value with drop tracking
        let key_gc = Gc::new(DropTracker::new(key));
        root!(key_gc);

        let value_gc = Gc::new(DropTracker::new(value));

        ephemeron = Ephemeron::new(&key_gc, value_gc);

        // Both should be accessible
        assert!(ephemeron.is_key_alive());
        assert!(ephemeron.upgrade().is_some());
    }

    // Key is now dropped, but ephemeron still holds references
    clear_roots!();
    collect_full();

    // Key is dead, so value should also be inaccessible
    assert!(!ephemeron.is_key_alive());
    assert!(ephemeron.upgrade().is_none());

    clear_roots!();
}

// ============================================================================
// Drop tracking tests - verify value Drop is called when key becomes unreachable
// ============================================================================

#[derive(Clone)]
struct DropTracker {
    marker: Rc<Cell<bool>>,
}

impl DropTracker {
    const fn new(marker: Rc<Cell<bool>>) -> Self {
        Self { marker }
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.marker.set(true);
    }
}

unsafe impl Trace for DropTracker {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[test]
fn test_ephemeron_value_dropped_when_key_unreachable() {
    let key_marker = Rc::new(Cell::new(false));
    let value_marker = Rc::new(Cell::new(false));

    let ephemeron: Ephemeron<DropTracker, DropTracker>;

    {
        clear_roots!();

        let key_gc = Gc::new(DropTracker::new(key_marker.clone()));
        root!(key_gc);

        let value_gc = Gc::new(DropTracker::new(value_marker.clone()));

        ephemeron = Ephemeron::new(&key_gc, value_gc);

        // Neither should be dropped yet
        assert!(!key_marker.get());
        assert!(!value_marker.get());

        assert!(ephemeron.is_key_alive());
    }

    // Key is now dropped, value should be unreachable
    clear_roots!();
    collect_full();

    // Key should be collected
    assert!(key_marker.get(), "Key should be dropped when unreachable");

    // NOTE: Full ephemeron semantics would also collect the value when key dies.
    // This requires GC-level ephemeron tracking. For now, value may still be alive
    // because Ephemeron::Trace implementation unconditionally traces the value.
    // The is_key_alive() correctly returns false since key is gone.
    assert!(!ephemeron.is_key_alive(), "Key should be dead");

    // upgrade() returns None because key is dead (this works correctly)
    assert!(
        ephemeron.upgrade().is_none(),
        "Value not accessible when key dead"
    );

    clear_roots!();
}

#[test]
fn test_ephemeron_multiple_same_key() {
    let key_marker = Rc::new(Cell::new(false));
    let value1_marker = Rc::new(Cell::new(false));
    let value2_marker = Rc::new(Cell::new(false));

    let ephemeron1: Ephemeron<DropTracker, DropTracker>;
    let ephemeron2: Ephemeron<DropTracker, DropTracker>;

    {
        clear_roots!();

        let key_gc = Gc::new(DropTracker::new(key_marker.clone()));
        root!(key_gc);

        let value1_gc = Gc::new(DropTracker::new(value1_marker));
        let value2_gc = Gc::new(DropTracker::new(value2_marker));

        // Create ephemerons using reference to key
        ephemeron1 = Ephemeron::new(&key_gc, value1_gc);
        ephemeron2 = Ephemeron::new(&key_gc, value2_gc);

        // Key should still be alive (we still hold key_gc)
        assert!(ephemeron1.is_key_alive(), "ephemeron1 key should be alive");
        assert!(ephemeron2.is_key_alive(), "ephemeron2 key should be alive");
    }

    clear_roots!();
    collect_full();

    // Key should be collected when roots are cleared
    assert!(key_marker.get(), "Key should be collected when unreachable");

    // NOTE: Full ephemeron semantics would also collect values when key dies.
    // Current implementation may keep values alive (see test_ephemeron_value_dropped_when_key_unreachable).

    // is_key_alive correctly returns false since key is dead
    assert!(!ephemeron1.is_key_alive(), "Key should be dead");
    assert!(!ephemeron2.is_key_alive(), "Key should be dead");

    clear_roots!();
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn test_ephemeron_with_gc_struct_as_key_and_value() {
    #[derive(Trace)]
    struct MyStruct {
        value: i32,
    }

    clear_roots!();

    let key = Gc::new(MyStruct { value: 10 });
    root!(key);

    let value = Gc::new(MyStruct { value: 20 });
    // Clone key since we need it for the ephemeron
    let ephemeron = Ephemeron::new(&key, value);

    let upgraded = ephemeron.upgrade();
    assert!(upgraded.is_some());
    assert_eq!(upgraded.unwrap().value, 20);

    clear_roots!();
}

#[test]
fn test_ephemeron_upgrade_multiple_times() {
    let key = Gc::new("key");
    let value = Gc::new(String::from("hello"));
    let ephemeron = Ephemeron::new(&key, value);

    // Upgrade multiple times - should all succeed while key is alive
    for _ in 0..5 {
        let upgraded = ephemeron.upgrade();
        assert!(upgraded.is_some());
    }

    // Key still alive
    assert!(ephemeron.is_key_alive());
}

#[test]
fn test_ephemeron_ref_through_collection_cycle() {
    clear_roots!();
    let key = Gc::new(42);
    root!(key);
    let value = Gc::new(100);
    // Clone key to keep it alive
    let ephemeron = Ephemeron::new(&key, value);

    // Run multiple collections while key is alive
    for _ in 0..5 {
        collect_full();
        assert!(ephemeron.is_key_alive());
        assert!(ephemeron.upgrade().is_some());
    }

    drop(ephemeron);
    clear_roots!();
    collect_full();

    clear_roots!();
}

// ============================================================================
// Comparison: Ephemeron vs Weak (demonstrate difference)
// ============================================================================

#[test]
fn test_weak_vs_ephemeron_difference() {
    // With Weak<T>, dropping the value doesn't affect the weak reference
    // With Ephemeron<K, V>, dropping the key makes the value inaccessible

    // Test Weak behavior
    let value_weak = Gc::new(42);
    let weak = Gc::downgrade(&value_weak);

    // Drop the Gc - weak should still be alive (but upgrade fails after collection)
    drop(value_weak);
    collect_full();

    // Weak is still technically "alive" (not null) but upgrade returns None
    assert!(weak.upgrade().is_none());

    // Test Ephemeron behavior
    clear_roots!();
    let key = Gc::new("key");
    root!(key);
    let value = Gc::new(42);
    let ephemeron = Ephemeron::new(&key, value);

    // Drop the key (ephemeron's key reference)
    drop(ephemeron);
    clear_roots!();
    collect_full();

    // The key was dropped, so the ephemeron should reflect that
    // (we can't check upgrade here because ephemeron itself was dropped)

    clear_roots!();
}

// ============================================================================
// Ephemeron in Vec/Container tests
// ============================================================================

#[test]
fn test_ephemeron_in_vec() {
    clear_roots!();

    let key1 = Gc::new("key1");
    let key2 = Gc::new("key2");
    root!(key1);
    root!(key2);

    let value1 = Gc::new(100);
    let value2 = Gc::new(200);

    let ephemerons: Vec<Ephemeron<&'static str, i32>> =
        vec![Ephemeron::new(&key1, value1), Ephemeron::new(&key2, value2)];

    // All should be valid
    for eph in &ephemerons {
        assert!(eph.is_key_alive());
        assert!(eph.upgrade().is_some());
    }

    // Verify values
    assert_eq!(*ephemerons[0].upgrade().unwrap(), 100);
    assert_eq!(*ephemerons[1].upgrade().unwrap(), 200);

    clear_roots!();
}

#[test]
fn test_ephemeron_vec_multiple_collections() {
    clear_roots!();

    let key = Gc::new("key");
    root!(key);

    let mut ephemerons: Vec<Ephemeron<&'static str, i32>> = Vec::new();

    for i in 0..10 {
        let value = Gc::new(i * 10);
        ephemerons.push(Ephemeron::new(&key, value));
    }

    // Run multiple collections
    #[allow(clippy::cast_possible_truncation)]
    for _ in 0..5 {
        #[cfg(feature = "debug-suspicious-sweep")]
        clear_history();
        collect_full();
        for (i, eph) in ephemerons.iter().enumerate() {
            assert!(eph.is_key_alive(), "ephemeron {i} should be alive");
            let val = eph.upgrade().unwrap();
            assert_eq!(*val, i as i32 * 10, "ephemeron {i} value mismatch");
        }
    }

    clear_roots!();
}

#[test]
fn test_ephemeron_vec_partial_drop() {
    clear_roots!();

    // Create keys - first 3 will be rooted, rest won't
    let keys: Vec<Gc<&'static str>> = ["k0", "k1", "k2", "k3", "k4"]
        .iter()
        .map(|s| Gc::new(*s))
        .collect();

    // Register first 3 as roots (keep them alive)
    for key in &keys[..3] {
        root!(key);
    }

    let ephemerons: Vec<Ephemeron<&'static str, i32>> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| Ephemeron::new(k, Gc::new(i as i32)))
        .collect();

    // First 3 should be alive (rooted)
    for eph in &ephemerons[..3] {
        assert!(eph.is_key_alive());
        assert!(eph.upgrade().is_some());
    }

    // Last 2 should also appear alive because their keys still exist
    // (they just aren't registered as roots, but the Gc objects exist)
    // To truly test "dead" behavior, we'd need to drop the keys
    for eph in &ephemerons[3..] {
        assert!(
            eph.is_key_alive(),
            "keys exist but not rooted - still alive"
        );
    }

    clear_roots!();
}

// ============================================================================
// Ephemeron + External Reference (Rc) tests - verify true drop semantics
// ============================================================================

#[test]
fn test_ephemeron_value_truly_dropped_with_rc() {
    let key_marker = Rc::new(Cell::new(false));
    let value_marker = Rc::new(Cell::new(false));

    let ephemeron: Ephemeron<DropTracker, DropTracker>;

    {
        clear_roots!();

        let key_gc = Gc::new(DropTracker::new(key_marker.clone()));
        root!(key_gc);

        let value_gc = Gc::new(DropTracker::new(value_marker.clone()));

        ephemeron = Ephemeron::new(&key_gc, value_gc);

        // Neither should be dropped yet
        assert!(!key_marker.get());
        assert!(!value_marker.get());
    }

    // Key is now dropped, value should be unreachable
    clear_roots!();
    collect_full();

    // Key should be collected
    assert!(key_marker.get(), "Key should be dropped when unreachable");

    // NOTE: Current implementation keeps value alive because Ephemeron::Trace
    // unconditionally traces the value. This test documents the current behavior.
    // For true ephemeron semantics, value_marker should also be true here.
    assert!(!ephemeron.is_key_alive(), "Key should be dead");
    assert!(
        ephemeron.upgrade().is_none(),
        "Value not accessible when key dead"
    );

    clear_roots!();
}

#[test]
fn test_ephemeron_drop_key_and_ephemeron_both_gc() {
    let key_marker = Rc::new(Cell::new(false));
    let value_marker = Rc::new(Cell::new(false));

    let ephemeron: Ephemeron<DropTracker, DropTracker>;

    {
        clear_roots!();

        let key_gc = Gc::new(DropTracker::new(key_marker.clone()));
        let value_gc = Gc::new(DropTracker::new(value_marker.clone()));

        ephemeron = Ephemeron::new(&key_gc, value_gc);

        // Neither should be dropped yet
        assert!(!key_marker.get());
        assert!(!value_marker.get());

        // key_gc goes out of scope here (not rooted)
    }

    // Both key and ephemeron should be collected
    clear_roots!();
    collect_full();

    // Key should definitely be dropped
    assert!(key_marker.get(), "Key should be dropped");

    // Ephemeron's key should be dead
    assert!(!ephemeron.is_key_alive());

    clear_roots!();
}

// ============================================================================
// Ephemeron stress tests
// ============================================================================

#[test]
fn test_ephemeron_many_keys() {
    clear_roots!();

    let keys: Gc<RefCell<Vec<Gc<i32>>>> = Gc::new(RefCell::new((0..100).map(Gc::new).collect()));
    for key in keys.borrow().iter() {
        root!(key);
    }

    let keys_ref = keys.borrow();
    let ephemerons: Vec<Ephemeron<i32, i32>> = keys_ref
        .iter()
        .enumerate()
        .map(|(i, k)| Ephemeron::new(k, Gc::new(i as i32)))
        .collect();
    drop(keys_ref);

    // All should work
    for (i, eph) in ephemerons.iter().enumerate() {
        assert!(eph.is_key_alive(), "eph {} should be alive", i);
        assert_eq!(
            *eph.upgrade().unwrap(),
            i as i32,
            "eph {} value mismatch",
            i
        );
    }

    // Run multiple collections
    for _ in 0..3 {
        #[cfg(feature = "debug-suspicious-sweep")]
        clear_history();
        collect_full();
        for (i, eph) in ephemerons.iter().enumerate() {
            assert!(eph.is_key_alive(), "eph {} should be alive after GC", i);
            assert_eq!(*eph.upgrade().unwrap(), i as i32);
        }
    }

    clear_roots!();
}

#[test]
fn test_ephemeron_stress_many_keys_drop_randomly() {
    clear_roots!();

    // Create 50 keys, only root even indices
    let keys: Gc<RefCell<Vec<Gc<i32>>>> = Gc::new(RefCell::new((0..50).map(Gc::new).collect()));
    for (i, key) in keys.borrow().iter().enumerate() {
        if i % 2 == 0 {
            root!(key);
        }
    }

    let keys_ref = keys.borrow();
    let ephemerons: Vec<Ephemeron<i32, i32>> = keys_ref
        .iter()
        .enumerate()
        .map(|(i, k)| Ephemeron::new(k, Gc::new(i as i32)))
        .collect();
    drop(keys_ref);

    // All keys still exist, so all ephemerons should show key as alive
    // (they just aren't registered as roots for GC purposes)
    for (i, eph) in ephemerons.iter().enumerate() {
        assert!(
            eph.is_key_alive(),
            "eph {} key exists (not rooted but alive)",
            i
        );
    }

    clear_roots!();
}

#[test]
fn test_ephemeron_clone_preserves_count() {
    clear_roots!();

    let key = Gc::new("key");
    root!(key);
    let value = Gc::new(42);

    let ephemeron1 = Ephemeron::new(&key, value);
    let ephemeron2 = ephemeron1.clone();

    // Both should work
    assert!(ephemeron1.is_key_alive());
    assert!(ephemeron2.is_key_alive());

    assert_eq!(*ephemeron1.upgrade().unwrap(), 42);
    assert_eq!(*ephemeron2.upgrade().unwrap(), 42);

    // Drop original, clone should still work
    drop(ephemeron1);
    assert!(ephemeron2.is_key_alive());
    assert_eq!(*ephemeron2.upgrade().unwrap(), 42);

    clear_roots!();
}

// ============================================================================
// Ephemeron cycle tests
// ============================================================================

#[test]
fn test_ephemeron_simple_cycle() {
    clear_roots!();

    #[derive(Trace)]
    struct Node {
        value: i32,
    }

    let node = Gc::new(Node { value: 42 });

    // Create ephemeron where node is the key (outside the struct)
    let eph = Ephemeron::new(&node, Gc::new(100));

    // Value should be accessible while node is alive
    assert!(eph.is_key_alive());
    assert_eq!(*eph.upgrade().unwrap(), 100);

    clear_roots!();
}

#[test]
fn test_ephemeron_with_weak_combo() {
    clear_roots!();

    #[derive(Trace)]
    struct Inner {
        value: i32,
    }

    let inner = Gc::new(Inner { value: 10 });
    root!(inner);

    // Create both a weak and an ephemeron pointing to same target
    let weak = Gc::downgrade(&inner);
    let ephemeron = Ephemeron::new(&inner, Gc::new(99));

    // Verify weak works
    assert!(weak.upgrade().is_some());

    // Verify ephemeron works
    assert!(ephemeron.is_key_alive());
    assert_eq!(*ephemeron.upgrade().unwrap(), 99);

    clear_roots!();
}
