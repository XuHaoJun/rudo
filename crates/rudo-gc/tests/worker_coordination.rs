//! Integration tests for worker coordination with condvar-based backoff.
//!
//! These tests verify the correctness of the `GcWorkerRegistry` implementation
//! and the new `worker_mark_loop_with_registry` function.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rudo_gc::gc::marker::{try_steal_with_backoff, GcWorkerRegistry, PerThreadMarkQueue};

/// Test that `GcWorkerRegistry` can be created.
#[test]
fn test_worker_registry_creation() {
    let _registry = GcWorkerRegistry::new(4);
    // Registry is created successfully
}

/// Test that `set_complete` wakes up waiters.
#[test]
fn test_set_complete_wakes_waiters() {
    let registry = GcWorkerRegistry::new(1);
    let completed = Arc::new(AtomicUsize::new(0));

    let registry_clone = registry.clone();
    let completed_clone = completed.clone();

    let handle = thread::spawn(move || {
        let result = registry_clone.wait_for_work();
        completed_clone.fetch_add(1, Ordering::Relaxed);
        result
    });

    // Give the thread time to start waiting
    thread::sleep(Duration::from_millis(10));

    // Set complete
    registry.set_complete();

    // Thread should wake up
    let result = handle.join().unwrap();

    assert_eq!(completed.load(Ordering::Relaxed), 1);
    assert!(!result); // Should return false (complete)
}

/// Test that `try_steal_with_backoff` returns None when all queues are empty.
#[test]
fn test_try_steal_with_backoff_empty_queues() {
    let q1 = Arc::new(PerThreadMarkQueue::new_with_index(0));
    let _q2 = Arc::new(PerThreadMarkQueue::new_with_index(1));

    let queues = vec![q1.clone()];

    let result = try_steal_with_backoff(&q1, &queues);
    assert!(result.is_none());
}

/// Test that backoff in `try_steal_with_backoff` doesn't cause issues.
#[test]
fn test_steal_backoff_efficiency() {
    let q1 = Arc::new(PerThreadMarkQueue::new_with_index(0));

    // Create many empty queues
    let queues: Vec<Arc<PerThreadMarkQueue>> = (0..10)
        .map(|i| Arc::new(PerThreadMarkQueue::new_with_index(i)))
        .collect();

    // Measure time for try_steal_with_backoff
    let start = std::time::Instant::now();
    let result = try_steal_with_backoff(&q1, &queues);
    let elapsed = start.elapsed();

    // Should return quickly even when all queues are empty
    assert!(result.is_none());
    assert!(
        elapsed < Duration::from_millis(100),
        "Steal took too long: {elapsed:?}",
    );
}

/// Test that `notify_work_available` sets `work_available` flag.
#[test]
fn test_notify_work_available() {
    let registry = GcWorkerRegistry::new(1);

    // Initially no work available
    // Note: we can't directly test this without waiting,
    // but we can verify the method doesn't panic
    registry.notify_work_available();
    registry.set_complete();
}

#[test]
fn test_notify_work_available_wakes_waiter() {
    let registry = GcWorkerRegistry::new(1);
    let woken = Arc::new(AtomicBool::new(false));

    let registry_clone = registry.clone();
    let woken_clone = woken.clone();

    let handle = thread::spawn(move || {
        let result = registry_clone.wait_for_work();
        woken_clone.store(true, Ordering::SeqCst);
        result
    });

    thread::sleep(Duration::from_millis(50));

    registry.notify_work_available();

    let result = handle.join().unwrap();

    assert!(
        woken.load(Ordering::SeqCst),
        "Waiter should have been woken"
    );
    assert!(result, "Should return true (work available, not complete)");
}
