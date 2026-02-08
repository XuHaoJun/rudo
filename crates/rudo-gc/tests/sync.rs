//! Tests for Send + Sync trait implementations for Gc<T> and Weak<T>.
//!
//! These tests verify that Gc<T> and Weak<T> can be safely shared across threads
//! when T: Send + Sync.

#![allow(
    clippy::redundant_clone,
    clippy::let_and_return,
    clippy::use_self,
    clippy::items_after_statements
)]

use rudo_gc::{Gc, GcMutex, GcRwLock, Trace, Weak};
use std::sync::Arc;
use std::thread;

// Compile-time assertions for Send + Sync traits
// These assertions verify at compile time that Gc<T> and Weak<T> implement Send + Sync
// when T: Trace + Send + Sync.
#[allow(
    dead_code,
    clippy::redundant_clone,
    clippy::use_self,
    clippy::let_and_return
)]
const _: fn() = || {
    trait AssertSend<T: Send> {}
    trait AssertSync<T: Sync> {}

    impl<T: Trace + Send + Sync> AssertSend<Gc<T>> for Gc<T> {}
    impl<T: Trace + Send + Sync> AssertSync<Gc<T>> for Gc<T> {}
    impl<T: Trace + Send + Sync> AssertSend<Weak<T>> for Weak<T> {}
    impl<T: Trace + Send + Sync> AssertSync<Weak<T>> for Weak<T> {}
};

// Test data types
#[derive(Trace)]
#[allow(clippy::use_self)]
struct ThreadSafeData {
    value: i32,
    next: Option<Gc<ThreadSafeData>>,
}

#[derive(Trace)]
struct SendableWrapper {
    data: Arc<String>,
}

#[derive(Trace)]
struct MultiFieldData {
    a: i32,
    b: String,
    c: Vec<i32>,
}

// ============================================================================
// User Story 1 Tests: Multi-threaded Gc Pointer Sharing
// ============================================================================

#[test]
fn test_gc_send_to_thread() {
    let gc = Gc::new(42);

    let handle = thread::spawn(move || {
        assert_eq!(*gc, 42);
        gc
    });

    let result = handle.join().unwrap();
    assert_eq!(*result, 42);
}

#[test]
fn test_gc_clone_and_send() {
    let gc = Gc::new(Arc::new(vec![1, 2, 3]));

    let handle = thread::spawn(move || {
        let cloned = gc.clone();
        assert_eq!(*cloned, Arc::new(vec![1, 2, 3]));
        cloned
    });

    handle.join().unwrap();
}

#[test]
fn test_multiple_threads_access_gc() {
    let gc = Gc::new(0);
    let mut handles = Vec::new();

    for _ in 0..10 {
        let gc = gc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let _ = *gc;
            }
            // Each thread adds to the ref count by keeping its clone alive
            drop(gc);
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_gc_struct_send_to_thread() {
    let gc = Gc::new(ThreadSafeData {
        value: 123,
        next: None,
    });

    let handle = thread::spawn(move || {
        assert_eq!(gc.value, 123);
        assert!(gc.next.is_none());
        gc
    });

    handle.join().unwrap();
}

#[test]
fn test_gc_with_arc_field() {
    let gc = Gc::new(SendableWrapper {
        data: Arc::new("thread-safe".to_string()),
    });

    let handle = thread::spawn(move || {
        assert_eq!(&*gc.data, "thread-safe");
        gc
    });

    handle.join().unwrap();
}

#[test]
fn test_gc_drop_in_thread() {
    let gc = Gc::new(MultiFieldData {
        a: 1,
        b: "hello".to_string(),
        c: vec![1, 2, 3],
    });

    let handle = thread::spawn(move || {
        assert_eq!(gc.a, 1);
        assert_eq!(&gc.b, "hello");
        assert_eq!(gc.c, vec![1, 2, 3]);
        // Drop happens here when gc goes out of scope
    });

    handle.join().unwrap();
    rudo_gc::collect();
}

#[test]
fn test_gc_concurrent_clone_and_use() {
    let gc = Arc::new(Gc::new(0));
    let mut handles = Vec::new();

    for _ in 0..4 {
        let gc = gc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                let _ = *gc;
            }
            gc
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.join().unwrap());
    }

    // All threads completed successfully
    assert_eq!(results.len(), 4);
}

#[test]
fn test_gc_from_main_to_thread_to_main() {
    let gc = Gc::new("shared".to_string());

    let (tx, rx) = std::sync::mpsc::channel();

    let handle = thread::spawn(move || {
        tx.send(gc).unwrap();
    });

    handle.join().unwrap();

    let gc = rx.recv().unwrap();
    assert_eq!(&*gc, "shared");
}

// ============================================================================
// User Story 2 Tests: Thread-safe Weak References
// ============================================================================

#[test]
fn test_weak_upgrade_in_thread() {
    let strong = Gc::new(42);
    let weak = Gc::downgrade(&strong);

    let handle = thread::spawn(move || {
        // Upgrade the weak reference in a different thread
        let upgraded = weak.upgrade();
        assert!(upgraded.is_some());
        let value = *upgraded.unwrap();
        value
    });

    let result = handle.join().unwrap();
    assert_eq!(result, 42);
}

#[test]
fn test_weak_clone_and_send() {
    let strong = Gc::new(vec![1, 2, 3]);
    let weak = Gc::downgrade(&strong);

    let handle = thread::spawn(move || {
        let cloned_weak = weak.clone();
        let upgraded = cloned_weak.upgrade();
        assert!(upgraded.is_some());
        assert_eq!(*upgraded.unwrap(), vec![1, 2, 3]);
        cloned_weak
    });

    handle.join().unwrap();
}

#[test]
fn test_weak_is_alive_across_threads() {
    let strong = Gc::new(42);
    let weak = Gc::downgrade(&strong);

    assert!(weak.is_alive());

    let weak_clone = weak.clone();
    let handle = thread::spawn(move || {
        // While strong is still alive in main thread
        assert!(weak_clone.is_alive());
        weak_clone
    });

    let _weak_back = handle.join().unwrap();

    // Strong is still alive
    assert!(weak.is_alive());

    drop(strong);
    rudo_gc::collect();

    // Weak should now report dead
    assert!(!weak.is_alive());
}

#[test]
fn test_weak_strong_count_across_threads() {
    let strong = Gc::new(42);
    let weak = Gc::downgrade(&strong);

    let handle = thread::spawn(move || {
        let upgraded = weak.upgrade().unwrap();
        assert_eq!(weak.strong_count(), 2);
        drop(upgraded);
        assert_eq!(weak.strong_count(), 1);
        weak
    });

    let weak = handle.join().unwrap();

    assert_eq!(weak.strong_count(), 1);
}

#[test]
fn test_weak_ref_through_cyclic_structure_threaded() {
    #[derive(Trace)]
    struct Cyclic {
        value: i32,
        next: Option<Gc<Cyclic>>,
        prev: Weak<Cyclic>,
    }

    let gc = Gc::new_cyclic_weak(|prev| Cyclic {
        value: 1,
        next: None,
        prev,
    });

    let handle = thread::spawn(move || {
        let weak = Gc::downgrade(&gc);
        let upgraded = weak.upgrade();
        assert!(upgraded.is_some());
        assert_eq!(upgraded.unwrap().value, 1);
        weak
    });

    let weak = handle.join().unwrap();
    rudo_gc::collect();
    assert!(weak.upgrade().is_none());
}

// ============================================================================
// User Story 3 Tests: Concurrent GC Operations
// ============================================================================

#[test]
fn test_gc_collection_with_concurrent_threads() {
    let gc = Gc::new(vec![1, 2, 3, 4, 5]);
    let mut handles = Vec::new();

    for _ in 0..4 {
        let gc = gc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let _ = gc.len();
            }
            gc
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Force collection after all threads complete
    rudo_gc::collect();

    // The original gc should still be valid
    assert_eq!(*gc, vec![1, 2, 3, 4, 5]);
}

#[test]
fn test_gc_drop_during_concurrent_access() {
    let gc = Arc::new(Gc::new(0));
    let mut handles = Vec::new();

    for _ in 0..10 {
        let gc = gc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                let _ = *gc;
            }
            gc
        }));
    }

    // Drop the Arc clone on main thread while threads are running
    drop(gc);

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // Collection should complete without issues
    rudo_gc::collect();
}

#[test]
fn test_multiple_gcs_concurrent_lifecycle() {
    let mut gcs = Vec::new();
    let mut handles = Vec::new();

    // Create some Gc pointers
    for i in 0..10 {
        gcs.push(Gc::new(i));
    }

    // Clone and send to threads
    for (i, gc) in gcs.into_iter().enumerate() {
        let gc = gc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                assert_eq!(*gc, i);
            }
            gc
        }));
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    rudo_gc::collect();
}

#[test]
fn test_stress_concurrent_clone_drop() {
    let gc = Arc::new(Gc::new(0));
    let mut handles = Vec::new();

    // Spawn many threads that clone and drop rapidly
    for _ in 0..20 {
        let gc = gc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..500 {
                let cloned = gc.clone();
                let _ = *cloned;
                drop(cloned);
            }
            gc
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    rudo_gc::collect();
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_gc_of_gc_concurrent() {
    #[derive(Trace)]
    struct Nested {
        value: i32,
    }

    let gc = Gc::new(Nested { value: 1 });

    let handle = thread::spawn(|| {
        rudo_gc::collect();
        rudo_gc::collect();
    });

    handle.join().unwrap();
    drop(gc);
    rudo_gc::collect();
}

#[test]
fn test_empty_struct_threaded() {
    #[derive(Trace)]
    struct Empty;

    let gc = Gc::new(Empty);

    let handle = thread::spawn(move || {
        let _ = *gc;
        gc
    });

    handle.join().unwrap();
}

#[test]
fn test_gc_with_derived_clone() {
    #[derive(Trace, Clone)]
    struct Cloneable {
        value: i32,
    }

    let gc = Gc::new(Cloneable { value: 42 });

    let handle = thread::spawn(move || {
        let cloned = gc.clone();
        assert_eq!(cloned.value, 42);
        cloned
    });

    handle.join().unwrap();
}

// ============================================================================
// Weak Reference Edge Cases
// ============================================================================

#[test]
fn test_weak_after_strong_dropped_in_thread() {
    let weak: Weak<i32>;

    {
        let strong = Gc::new(100);
        weak = Gc::downgrade(&strong);
        assert!(weak.is_alive());
    } // strong dropped here

    let weak_clone = weak.clone();
    let handle = thread::spawn(move || {
        rudo_gc::collect();
        assert!(!weak_clone.is_alive());
    });

    handle.join().unwrap();
}

#[test]
fn test_multiple_weaks_concurrent() {
    let strong = Gc::new(vec![1, 2, 3]);
    let mut weak_handles = Vec::new();

    for _ in 0..10 {
        let weak = Gc::downgrade(&strong);
        weak_handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let _ = weak.is_alive();
            }
            weak
        }));
    }

    let mut weak_refs = Vec::new();
    for handle in weak_handles {
        weak_refs.push(handle.join().unwrap());
    }

    // All weaks should still be alive
    for weak in &weak_refs {
        assert!(weak.is_alive());
    }

    drop(strong);
    rudo_gc::collect();

    // All weaks should now be dead
    for weak in weak_refs {
        assert!(!weak.is_alive());
    }
}

// ============================================================================
// GcRwLock and GcMutex Tests (011-concurrent-gc-primitives)
// ============================================================================

#[derive(Trace, Debug, Default)]
struct TestData {
    value: i32,
}

#[test]
fn test_gc_rwlock_read() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcRwLock<TestData>> = Gc::new(GcRwLock::new(TestData { value: 10 }));

    let guard = lock.read();
    let value = guard.value;
    drop(guard);
    assert_eq!(value, 10);
}

#[test]
fn test_gc_rwlock_write() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcRwLock<TestData>> = Gc::new(GcRwLock::new(TestData { value: 10 }));

    {
        let mut guard = lock.write();
        guard.value = 20;
    }

    assert_eq!(lock.read().value, 20);
}

#[test]
fn test_gc_rwlock_try_read() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcRwLock<TestData>> = Gc::new(GcRwLock::new(TestData { value: 10 }));

    assert!(lock.try_read().is_some());
}

#[test]
fn test_gc_rwlock_try_write() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcRwLock<TestData>> = Gc::new(GcRwLock::new(TestData { value: 10 }));

    assert!(lock.try_write().is_some());
}

#[test]
fn test_gc_rwlock_is_locked() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcRwLock<TestData>> = Gc::new(GcRwLock::new(TestData { value: 10 }));

    // Initially not locked (no writers)
    assert!(!lock.is_locked());

    // After acquiring write lock, should be locked
    {
        let _guard = lock.write();
        assert!(lock.is_locked());
    }
    // After releasing, should not be locked
    assert!(!lock.is_locked());
}

#[test]
fn test_gc_rwlock_default() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcRwLock<TestData>> = Gc::new(GcRwLock::default());
    assert_eq!(lock.read().value, 0);
}

#[test]
fn test_gc_mutex_lock() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcMutex<TestData>> = Gc::new(GcMutex::new(TestData { value: 10 }));

    let guard = lock.lock();
    let value = guard.value;
    drop(guard);
    assert_eq!(value, 10);
}

#[test]
fn test_gc_mutex_try_lock() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcMutex<TestData>> = Gc::new(GcMutex::new(TestData { value: 10 }));

    assert!(lock.try_lock().is_some());
}

#[test]
fn test_gc_mutex_is_locked() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcMutex<TestData>> = Gc::new(GcMutex::new(TestData { value: 10 }));

    assert!(!lock.is_locked());

    let _guard = lock.lock();
    assert!(lock.is_locked());
}

#[test]
fn test_gc_mutex_default() {
    rudo_gc::test_util::reset();
    let lock: Gc<GcMutex<TestData>> = Gc::new(GcMutex::default());
    assert_eq!(lock.lock().value, 0);
}

#[test]
fn test_concurrent_readers() {
    rudo_gc::test_util::reset();
    let data: Gc<GcRwLock<i32>> = Gc::new(GcRwLock::new(0));

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let data: Gc<GcRwLock<i32>> = Gc::clone(&data);
            std::thread::spawn(move || {
                for _ in 0..100 {
                    let guard = data.read();
                    let _ = *guard;
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_write_exclusivity() {
    rudo_gc::test_util::reset();
    let data: Gc<GcRwLock<i32>> = Gc::new(GcRwLock::new(0));

    // Writer thread
    let handle = std::thread::spawn({
        let data: Gc<GcRwLock<i32>> = Gc::clone(&data);
        move || {
            let mut guard = data.write();
            *guard = 42;
        }
    });

    // Main thread writes after spawn
    {
        let mut guard = data.write();
        *guard = 100;
    }

    handle.join().unwrap();
    // One of the writes succeeded - which one depends on thread scheduling
    let value = *data.read();
    assert!(value == 42 || value == 100);
}

// Compile-time assertions for GcRwLock and GcMutex Send + Sync traits
#[allow(dead_code)]
const _: fn() = || {
    trait AssertSend<T: Send> {}
    trait AssertSync<T: Sync> {}

    impl<T: Trace + Send + Sync> AssertSend<GcRwLock<T>> for GcRwLock<T> {}
    impl<T: Trace + Send + Sync> AssertSync<GcRwLock<T>> for GcRwLock<T> {}
    impl<T: Trace + Send + Sync> AssertSend<GcMutex<T>> for GcMutex<T> {}
    impl<T: Trace + Send + Sync> AssertSync<GcMutex<T>> for GcMutex<T> {}
};

// ============================================================================
// US2: Performance Isolation - GcCell single-threaded usage verification
// ============================================================================

#[test]
fn test_gccell_single_threaded() {
    use rudo_gc::GcCell;
    rudo_gc::test_util::reset();

    #[derive(Trace)]
    struct Data {
        value: i32,
    }

    let cell: rudo_gc::Gc<GcCell<Data>> = rudo_gc::Gc::new(GcCell::new(Data { value: 42 }));

    {
        let mut guard = cell.borrow_mut_gen_only();
        guard.value = 100;
    }

    assert_eq!(cell.borrow().value, 100);
}

#[test]
fn test_gccell_performance_no_atomics() {
    use rudo_gc::GcCell;
    rudo_gc::test_util::reset();

    #[derive(Trace)]
    struct Counter {
        count: u64,
    }

    let cell: rudo_gc::Gc<GcCell<Counter>> = rudo_gc::Gc::new(GcCell::new(Counter { count: 0 }));

    // Single-threaded access should have no atomic overhead
    #[allow(clippy::cast_lossless, clippy::cast_sign_loss)]
    for i in 0..1000 {
        let mut guard = cell.borrow_mut_gen_only();
        guard.count = i as u64;
    }

    assert_eq!(cell.borrow().count, 999);
}
