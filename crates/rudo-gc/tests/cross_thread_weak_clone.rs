//! Tests for `WeakCrossThreadHandle` clone behavior.
//!
//! These tests verify that cloning a `WeakCrossThreadHandle` correctly
//! increments the weak reference count.

#![allow(clippy::redundant_clone)]

use std::thread;

use rudo_gc::gc;
use rudo_gc::{Gc, Trace};

#[derive(Trace, Debug)]
struct TestData {
    value: i32,
}

/// Test that cloning `WeakCrossThreadHandle` increments weak count.
#[test]
fn test_weak_clone_increments_count() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak1 = gc.weak_cross_thread_handle();

    assert_eq!(
        Gc::weak_count(&gc),
        1,
        "Initial weak count should be 1 after downgrade"
    );

    let weak2 = weak1.clone();

    assert_eq!(
        Gc::weak_count(&gc),
        2,
        "Weak count should be 2 after cloning weak handle"
    );

    drop(weak1);

    assert_eq!(
        Gc::weak_count(&gc),
        1,
        "Weak count should be 1 after dropping one weak handle"
    );

    drop(weak2);

    gc::collect();

    assert_eq!(
        Gc::weak_count(&gc),
        0,
        "Weak count should be 0 after dropping all weak handles"
    );
}

/// Simple test: cloned weak handle allows resurrection after strong handle is dropped
#[test]
fn test_weak_clone_simple_liveness() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak1 = gc.weak_cross_thread_handle();

    let weak2 = weak1.clone();

    drop(gc);

    gc::collect();

    let resolved = weak2.resolve();
    assert!(
        resolved.is_some(),
        "weak2 should resolve - object memory should be retained while weak refs exist"
    );
    assert_eq!(resolved.as_ref().unwrap().value, 42);

    drop(resolved);
    drop(weak2);
}

/// Test that `resolve()` works on cloned weak handle.
#[test]
fn test_weak_clone_resolve() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 77 });
    let weak1 = gc.weak_cross_thread_handle();
    let weak2 = weak1.clone();

    let resolved1 = weak1.resolve();
    let resolved2 = weak2.resolve();

    assert!(
        resolved1.is_some() && resolved2.is_some(),
        "Both original and cloned weak should resolve successfully"
    );

    assert_eq!(
        resolved1.as_ref().unwrap().value,
        77,
        "Original weak resolve should have correct value"
    );
    assert_eq!(
        resolved2.as_ref().unwrap().value,
        77,
        "Cloned weak resolve should have correct value"
    );

    drop(resolved1);
    drop(resolved2);
    drop(gc);

    gc::collect();

    let resolved_after_drop = weak2.resolve();
    assert!(
        resolved_after_drop.is_none(),
        "Resolve should fail after object is collected"
    );
}

/// Test multiple clones of weak handle.
#[test]
fn test_multiple_weak_clones() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 100 });
    let weak1 = gc.weak_cross_thread_handle();
    let weak2 = weak1.clone();
    let weak3 = weak2.clone();

    assert_eq!(
        Gc::weak_count(&gc),
        3,
        "Should have 3 weak refs after creating 3 clones"
    );

    let weak4 = weak1.clone();
    assert_eq!(
        Gc::weak_count(&gc),
        4,
        "Should have 4 weak refs after cloning again"
    );

    drop(weak2);
    assert_eq!(
        Gc::weak_count(&gc),
        3,
        "Should have 3 weak refs after dropping one"
    );

    let resolved3 = weak3.resolve();
    let resolved4 = weak4.resolve();
    assert!(
        resolved3.is_some() && resolved4.is_some(),
        "Both remaining clones should still resolve"
    );

    drop(resolved3);
    drop(resolved4);
    drop(weak3);
    drop(weak4);

    gc::collect();
}

/// Regression test: cloned weak handle should not cause premature collection.
#[test]
fn test_weak_clone_no_premature_collection() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 100 });
    let weak1 = gc.weak_cross_thread_handle();

    let _weak2 = weak1.clone();
    let weak3 = weak1.clone();

    drop(gc);

    gc::collect();

    let resolved3 = weak3.resolve();
    assert!(resolved3.is_some(), "weak3 should resolve");

    drop(resolved3);
    drop(weak3);

    gc::collect();
}

/// Test weak handle clone across threads.
#[test]
fn test_weak_clone_across_threads() {
    use std::sync::mpsc;

    let gc: Gc<TestData> = Gc::new(TestData { value: 99 });
    let weak_original = gc.weak_cross_thread_handle();

    assert_eq!(Gc::weak_count(&gc), 1);

    let (sender, receiver) = mpsc::channel();

    let sender_thread = thread::spawn(move || {
        let weak_clone = weak_original.clone();
        sender.send(weak_clone).unwrap();
    });

    sender_thread.join().unwrap();

    let weak_received = receiver.recv().unwrap();

    drop(gc);

    gc::collect();

    let resolved = weak_received.try_resolve();
    assert!(
        resolved.is_none(),
        "try_resolve from wrong thread should return None"
    );

    drop(weak_received);

    gc::collect();
}
