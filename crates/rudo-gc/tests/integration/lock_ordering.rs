//! Integration tests for lock ordering discipline.
//!
//! These tests verify that the lock ordering discipline is correctly enforced
//! and that deadlocks do not occur under concurrent access patterns.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use rudo_gc::gc::sync::{LockGuard, LockOrder};

/// Test that lock order constants have correct values.
#[test]
fn test_lock_order_constants() {
    assert_eq!(LockOrder::LocalHeap.order_value(), 1);
    assert_eq!(LockOrder::GlobalMarkState.order_value(), 2);
    assert_eq!(LockOrder::GcRequest.order_value(), 3);
}

/// Test that lock order comparisons work correctly.
#[test]
fn test_lock_order_comparison() {
    assert!(LockOrder::LocalHeap.order_value() < LockOrder::GlobalMarkState.order_value());
    assert!(LockOrder::GlobalMarkState.order_value() < LockOrder::GcRequest.order_value());
    assert!(LockOrder::LocalHeap.order_value() < LockOrder::GcRequest.order_value());
}

/// Test LockGuard RAII guard for lock ordering.
#[test]
fn test_lock_guard_creation() {
    let _guard = LockGuard::new(LockOrder::LocalHeap);
    // Guard should be created without panic
}

/// Test concurrent lock acquisition doesn't cause deadlocks.
///
/// This test spawns multiple threads that each acquire locks in the
/// correct order (LocalHeap → GlobalMarkState → GcRequest).
/// If lock ordering is violated, deadlocks may occur.
#[test]
fn test_concurrent_lock_acquisition_no_deadlock() {
    let num_threads = 4;
    let iterations = 10;

    let barrier = Arc::new(Barrier::new(num_threads));
    let completed = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let completed = Arc::clone(&completed);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..iterations {
                    // Acquire locks in correct order
                    let _guard1 = LockGuard::new(LockOrder::LocalHeap);
                    let _guard2 = LockGuard::new(LockOrder::GlobalMarkState);
                    let _guard3 = LockGuard::new(LockOrder::GcRequest);

                    // Small delay to increase contention
                    thread::sleep(Duration::from_micros(100));

                    completed.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // All iterations should complete
    let expected = num_threads * iterations;
    assert_eq!(completed.load(Ordering::SeqCst), expected);
}

/// Test thread registry access with lock ordering.
///
/// This test verifies that accessing the thread registry with lock ordering
/// validation doesn't cause issues in normal operation.
#[test]
fn test_thread_registry_with_lock_ordering() {
    // Access thread registry multiple times
    for _ in 0..10 {
        let registry = rudo_gc::heap::thread_registry();
        let _guard = registry.lock().unwrap();

        // Small delay to simulate work
        thread::sleep(Duration::from_micros(10));
    }
}

/// Test that multiple threads can safely access thread registry concurrently.
#[test]
fn test_concurrent_thread_registry_access() {
    let num_threads = 8;
    let iterations = 50;

    let barrier = Arc::new(Barrier::new(num_threads));
    let errors = Arc::new(Mutex::new(Vec::new()));

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let barrier = Arc::clone(&barrier);
            let errors = Arc::clone(&errors);
            thread::spawn(move || {
                barrier.wait();
                for i in 0..iterations {
                    let result = std::panic::catch_unwind(|| {
                        let registry = rudo_gc::heap::thread_registry();
                        let _guard = registry.lock().unwrap();

                        // Simulate some work
                        if i % 10 == 0 {
                            thread::sleep(Duration::from_micros(1));
                        }
                    });

                    if result.is_err() {
                        let mut errors = errors.lock().unwrap();
                        errors.push(format!("Thread {} iteration {} panicked", thread_id, i));
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    let errors = errors.lock().unwrap();
    assert!(errors.is_empty(), "Panics occurred: {:?}", errors);
}

/// Test that lock ordering validation works in debug builds.
///
/// In debug builds, validate_lock_order should panic if lock order is violated.
/// In release builds, it should be a no-op.
#[test]
#[should_panic(expected = "Lock ordering violation")]
fn test_lock_ordering_validation_panics_on_violation() {
    // This test only runs in debug builds where validation is enabled
    #[cfg(debug_assertions)]
    {
        use rudo_gc::gc::sync::validate_lock_order;

        // Trying to acquire GcRequest (order 3) while holding LocalHeap (order 1) as minimum
        // should panic
        validate_lock_order(LockOrder::GcRequest, LockOrder::LocalHeap);
    }

    // In release builds, this test is skipped
    #[cfg(not(debug_assertions))]
    {
        panic!("This test only runs in debug builds");
    }
}

/// Test stress test for concurrent operations.
///
/// This test stresses the concurrent marking system to ensure no deadlocks
/// occur under high contention.
#[test]
fn test_stress_concurrent_operations() {
    let num_threads = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);

    let barrier = Arc::new(Barrier::new(num_threads));
    let completed = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let barrier = Arc::clone(&barrier);
            let completed = Arc::clone(&completed);
            thread::spawn(move || {
                // Each thread does a mix of operations
                for i in 0..100 {
                    // Access thread registry
                    let registry = rudo_gc::heap::thread_registry();
                    let _guard = registry.lock().unwrap();

                    // Use lock guard
                    let _lg = LockGuard::new(LockOrder::GlobalMarkState);

                    // Barrier synchronization
                    if i % 10 == 0 {
                        barrier.wait();
                    }

                    completed.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // All operations should complete
    let expected = num_threads * 100;
    assert_eq!(completed.load(Ordering::SeqCst), expected);
}
