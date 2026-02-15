//! Tests for weak cross-thread upgrade behavior.
//!
//! These tests verify that `WeakCrossThreadHandle` can correctly upgrade
//! in various scenarios.

use std::thread;

use rudo_gc::gc::collect_full;
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct TestData {
    value: i32,
}

#[test]
fn test_weak_upgrade_in_origin_thread_after_gc_drop() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak = gc.weak_cross_thread_handle();

    drop(gc);

    // Try upgrade in the same thread - this might trigger GC via notify_dropped_gc
    let result = weak.resolve();
    // The result depends on whether GC ran and set DEAD_FLAG
    // This is expected behavior - we just verify it doesn't panic
    drop(result);
    drop(weak);
}

#[test]
fn test_weak_clone_after_gc() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 99 });
    let weak1 = gc.weak_cross_thread_handle();
    let weak2 = weak1.clone();

    drop(gc);
    collect_full();

    // After GC, weak should not be able to upgrade because value was dropped
    let result1 = weak1.resolve();
    let result2 = weak2.resolve();

    assert!(
        result1.is_none() && result2.is_none(),
        "Both weak refs should return None after GC runs"
    );
}

#[test]
fn test_weak_invalid_after_gc_no_weak_refs() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    // Use regular Weak, not cross-thread handle
    let weak = Gc::downgrade(&gc);

    drop(gc);
    collect_full();

    let result = weak.upgrade();
    assert!(
        result.is_none(),
        "Weak with no explicit weak refs should return None after GC"
    );
}

#[test]
fn test_weak_cross_thread_handle_after_gc() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak = gc.weak_cross_thread_handle();

    drop(gc);
    collect_full();

    // After GC, weak should not be able to upgrade because value was dropped
    let result = weak.resolve();
    assert!(
        result.is_none(),
        "WeakCrossThreadHandle should return None after GC runs"
    );
}

#[test]
fn test_try_resolve_wrong_thread_returns_none() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak = gc.weak_cross_thread_handle();

    drop(gc);

    // try_resolve should return None when called from wrong thread
    let weak_clone = weak.clone();
    let result = thread::spawn(move || weak_clone.try_resolve())
        .join()
        .unwrap();

    assert!(
        result.is_none(),
        "try_resolve should return None when called from wrong thread"
    );

    drop(weak);
}
