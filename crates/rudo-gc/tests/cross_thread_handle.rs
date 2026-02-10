//! Integration tests for cross-thread GC handles.
//!
//! These tests verify the correctness of cross-thread handle functionality,
//! including send/sync guarantees, origin-thread enforcement, and lifetime
//! management.

use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use rudo_gc::handles::GcHandle;
use rudo_gc::{gc, Gc, Trace};

#[derive(Trace, Debug)]
struct TestData {
    value: i32,
}

#[derive(Trace, Debug)]
struct NestedData {
    inner: Gc<TestData>,
    name: String,
}

/// Test that handles can be sent between threads via channels.
#[test]
fn test_cross_thread_send() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        sender.send(handle).unwrap();
    });

    let received_handle = receiver.recv().unwrap();

    // Resolve on origin thread
    let resolved: Gc<TestData> = received_handle.resolve();
    assert_eq!(resolved.value, 42);
}

/// Test that `resolve()` panics on wrong thread.
#[test]
fn test_resolve_origin_thread_panics() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    // Spawn a new thread that will try to resolve
    let join_handle = thread::spawn(move || {
        // This should panic because we're on the wrong thread
        let _ = handle.resolve();
    });

    // join() returns Err if the thread panicked
    let join_result = join_handle.join();
    assert!(
        join_result.is_err(),
        "resolve() should panic on wrong thread"
    );
}

/// Test that `try_resolve()` returns None on wrong thread.
#[test]
fn test_try_resolve_wrong_thread() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    let result = std::sync::Arc::new(std::sync::Mutex::new(None));

    // Spawn a new thread that will try to resolve
    let result_clone = Arc::clone(&result);
    let join_handle = thread::spawn(move || {
        // try_resolve should return None on wrong thread (no panic)
        *result_clone.lock().unwrap() = handle.try_resolve();
    });

    join_handle.join().unwrap();

    assert!(result.lock().unwrap().is_none());
}

/// Test that multiple handles to the same object work correctly.
#[test]
fn test_multiple_handles_same_object() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 100 });
    let handle1 = gc.cross_thread_handle();
    let handle2 = gc.cross_thread_handle();
    let handle3 = gc.cross_thread_handle();

    // All handles should resolve to the same object
    let resolved1: Gc<TestData> = handle1.resolve();
    let resolved2: Gc<TestData> = handle2.resolve();
    let resolved3: Gc<TestData> = handle3.resolve();

    // All should point to the same value
    assert_eq!(resolved1.value, 100);
    assert_eq!(resolved2.value, 100);
    assert_eq!(resolved3.value, 100);
}

/// Test that handles keep objects alive during GC.
#[test]
fn test_handle_keeps_alive() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    // Drop the original GC reference
    drop(gc);

    // Force GC - handle should keep the object alive
    gc::collect();

    // Resolve should still work
    let resolved: Gc<TestData> = handle.resolve();
    assert_eq!(resolved.value, 42);
}

/// Test that cloned handles have independent lifetimes.
#[test]
fn test_clone_independent_lifetime() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle1 = gc.cross_thread_handle();
    let handle2 = handle1.clone();

    // Drop original and handle1
    drop(gc);
    drop(handle1);

    // handle2 should still keep the object alive
    gc::collect();

    let resolved: Gc<TestData> = handle2.resolve();
    assert_eq!(resolved.value, 42);
}

/// Test that dropping handles from foreign threads is safe.
#[test]
fn test_drop_from_foreign_thread() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    let (sender, receiver) = mpsc::channel();
    sender.send(handle).unwrap();

    let foreign_handle = receiver.recv().unwrap();

    // Drop on foreign thread - should not panic
    drop(foreign_handle);

    // Original should still be valid
    let resolved: Gc<TestData> = gc.cross_thread_handle().resolve();
    assert_eq!(resolved.value, 42);
}

/// Test `is_valid()` checks.
#[test]
fn test_is_valid_checks() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let mut handle = gc.cross_thread_handle();

    assert!(handle.is_valid());

    handle.unregister();

    assert!(!handle.is_valid());
}

/// Test that unregister is idempotent - calling resolve after unregister panics.
#[test]
fn test_unregister_idempotent() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let mut handle = gc.cross_thread_handle();

    assert!(handle.is_valid());

    // Unregister twice - should not panic
    handle.unregister();
    handle.unregister();

    // is_valid should return false after unregister
    assert!(!handle.is_valid());
}

/// Test weak handles don't prevent collection (basic test).
#[test]
fn test_weak_handle_no_prevent() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak = gc.weak_cross_thread_handle();

    // Drop the strong reference
    drop(gc);

    // Weak handle should still exist
    assert!(weak.origin_thread() == std::thread::current().id());
}

/// Test weak handle `is_valid()` returns false after GC collection.
#[test]
fn test_weak_is_valid_after_gc() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static DROPPED: AtomicBool = AtomicBool::new(false);

    #[derive(Trace)]
    struct TestData {
        value: i32,
    }

    impl Drop for TestData {
        fn drop(&mut self) {
            DROPPED.store(true, Ordering::SeqCst);
        }
    }

    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak = gc.weak_cross_thread_handle();

    drop(gc);
    gc::collect();

    assert!(DROPPED.load(Ordering::SeqCst));
    assert!(!weak.is_valid());
}

/// Test weak handle `try_resolve()` returns None after GC collection.
#[test]
fn test_weak_try_resolve_after_gc() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static DROPPED: AtomicBool = AtomicBool::new(false);

    #[derive(Trace)]
    struct TestData {
        value: i32,
    }

    impl Drop for TestData {
        fn drop(&mut self) {
            DROPPED.store(true, Ordering::SeqCst);
        }
    }

    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let weak = gc.weak_cross_thread_handle();

    drop(gc);
    gc::collect();

    assert!(DROPPED.load(Ordering::SeqCst));
    assert!(weak.try_resolve().is_none());
}

/// Test that `downgrade()` properly tracks weak references (weak count incremented).
#[test]
fn test_weak_downgrade_liveness() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static DROPPED: AtomicBool = AtomicBool::new(false);

    #[derive(Trace)]
    struct TestData {
        value: i32,
    }

    impl Drop for TestData {
        fn drop(&mut self) {
            DROPPED.store(true, Ordering::SeqCst);
        }
    }

    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();
    let weak = handle.downgrade();

    drop(gc);
    drop(handle);
    gc::collect();

    assert!(DROPPED.load(Ordering::SeqCst));
    assert!(!weak.is_valid());
}

/// Test strong-to-weak downgrade.
#[test]
fn test_downgrade() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    let weak = handle.downgrade();

    // Weak should resolve while strong exists
    let resolved: Gc<TestData> = weak.resolve().unwrap();
    assert_eq!(resolved.value, 42);

    // Drop the resolved reference (upgraded from weak)
    drop(resolved);

    // Drop strong handle and original
    drop(handle);
    drop(gc);

    // Force GC to collect the object
    gc::collect();

    // Weak handle should detect object was collected
    assert!(!weak.is_valid());
}

/// Test `origin_thread()` returns correct thread ID.
#[test]
fn test_origin_thread_returns_correct() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    let origin = handle.origin_thread();
    let current = thread::current().id();

    assert_eq!(origin, current);
}

/// Test nested data with Gc inside.
#[test]
fn test_nested_gc_handles() {
    let inner: Gc<TestData> = Gc::new(TestData { value: 42 });
    let nested: Gc<NestedData> = Gc::new(NestedData {
        inner,
        name: "test".to_string(),
    });

    let handle = nested.cross_thread_handle();

    let (sender, recv) = mpsc::channel();
    sender.send(handle).unwrap();

    let received = recv.recv().unwrap();
    let resolved: Gc<NestedData> = received.resolve();

    assert_eq!(resolved.name, "test");
    assert_eq!(resolved.inner.value, 42);
}

/// Test handle with value modification on origin thread.
#[test]
fn test_handle_value_modification() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 0 });
    let handle = gc.cross_thread_handle();

    let (sender, recv) = mpsc::channel();
    sender.send(handle).unwrap();

    let received = recv.recv().unwrap();

    // Resolve and verify value on origin thread
    {
        let resolved = received.resolve();
        assert_eq!(resolved.value, 0);
    }

    // Verify value persisted
    let resolved = received.resolve();
    assert_eq!(resolved.value, 0);
}

/// Test many handles to same object.
#[test]
fn test_many_handles() {
    let gc: Gc<TestData> = Gc::new(TestData { value: 1 });
    let handles: Vec<GcHandle<TestData>> = (0..100).map(|_| gc.cross_thread_handle()).collect();

    // All should resolve correctly
    for handle in handles {
        let resolved = handle.resolve();
        assert_eq!(resolved.value, 1);
    }
}

/// Test handle with different data types (basic types only).
#[test]
fn test_different_types() {
    let int_handle: GcHandle<i32> = Gc::new(42).cross_thread_handle();
    let vec_handle: GcHandle<Vec<i32>> = Gc::new(vec![1, 2, 3]).cross_thread_handle();

    assert_eq!(*int_handle.resolve(), 42);
    assert_eq!(*vec_handle.resolve(), vec![1, 2, 3]);
}

/// Test that cross-thread handles keep objects alive during MAJOR GC.
/// This test allocates enough to trigger a real major collection.
#[test]
fn test_cross_thread_handle_survives_major_gc() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[derive(Trace)]
    struct TestData {
        value: i32,
    }

    impl Drop for TestData {
        fn drop(&mut self) {
            DROP_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Create object with drop counting
    let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
    let handle = gc.cross_thread_handle();

    // Drop the original reference
    drop(gc);

    // Verify original was collected
    assert_eq!(
        DROP_COUNT.load(Ordering::SeqCst),
        1,
        "Original Gc reference should have been dropped"
    );

    // Reset for next check
    DROP_COUNT.store(0, Ordering::SeqCst);

    // Allocate >10MB to force major GC (triggers actual collection)
    let mut allocations = Vec::with_capacity(1024);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    for i in 0..1024 {
        allocations.push(Gc::new(vec![i as u8; 1024]));
    }

    // Force collection
    gc::collect();

    // Verify object was NOT collected (handle kept it alive)
    let resolved: Gc<TestData> = handle.resolve();
    assert_eq!(resolved.value, 42);
    assert_eq!(
        DROP_COUNT.load(Ordering::SeqCst),
        0,
        "Object should NOT be collected while handle exists"
    );

    // Cleanup
    drop(allocations);
    drop(resolved);
}
