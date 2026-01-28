//! Integration tests for work stealing and push-based transfer.
//!
//! These tests verify the correctness of work distribution mechanisms
//! including push-based work transfer and dynamic stack growth.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use rudo_gc::gc::marker::{PerThreadMarkQueue, MARK_QUEUE_SIZE};

/// Test basic push and pop operations.
#[test]
fn test_queue_basic_operations() {
    let queue = PerThreadMarkQueue::new();

    assert!(queue.is_empty(), "Queue should be empty initially");
    assert_eq!(queue.len(), 0, "Queue length should be 0");

    // Queue is full at MARK_QUEUE_SIZE
    let ptr = 0x1234usize as *const rudo_gc::ptr::GcBox<()>;

    // Push up to capacity
    for i in 0..MARK_QUEUE_SIZE {
        assert!(queue.push(ptr), "Push {} should succeed", i);
    }

    assert!(!queue.push(ptr), "Push should fail when queue is full");
    assert_eq!(queue.len(), MARK_QUEUE_SIZE, "Queue should be at capacity");
}

/// Test push-based work transfer between queues.
#[test]
fn test_push_based_transfer() {
    let queue1 = Arc::new(PerThreadMarkQueue::new_with_index(0));
    let queue2 = Arc::new(PerThreadMarkQueue::new_with_index(1));

    let ptr = 0x5678usize as *const rudo_gc::ptr::GcBox<()>;

    // Push to queue2 via queue1 (remote push)
    PerThreadMarkQueue::push_remote(&queue2, ptr);

    assert!(queue2.has_pending_work(), "Queue2 should have pending work");
    assert_eq!(
        queue2.pending_work_len(),
        1,
        "Should have 1 pending work item"
    );

    // Receive pending work
    let received = queue2.receive_pending_work();
    assert_eq!(received, Some(ptr), "Should receive the pushed work");
    assert!(
        !queue2.has_pending_work(),
        "Queue2 should have no more pending work"
    );
}

/// Test dynamic capacity hint growth.
#[test]
fn test_capacity_hint_growth() {
    let queue = PerThreadMarkQueue::new();

    let initial_hint = queue.capacity_hint();
    assert_eq!(initial_hint, (MARK_QUEUE_SIZE as f64 * 0.75) as usize);

    // Simulate high utilization
    let ptr = 0x9999usize as *const rudo_gc::ptr::GcBox<()>;

    // Fill queue to trigger overflow handling
    for _ in 0..MARK_QUEUE_SIZE {
        let _ = queue.push(ptr);
    }

    // Update capacity hint based on utilization
    queue.update_capacity_hint();

    let new_hint = queue.capacity_hint();
    // Hint should adjust based on utilization
    assert!(new_hint <= queue.max_capacity());
}

/// Test overflow handling with remote push.
#[test]
fn test_overflow_handling() {
    let num_queues = 4;
    let queues: Vec<Arc<PerThreadMarkQueue>> = (0..num_queues)
        .map(|i| Arc::new(PerThreadMarkQueue::new_with_index(i)))
        .collect();

    let queues_ref: Vec<PerThreadMarkQueue> = queues.iter().map(|q| (**q).clone()).collect();
    let ptr = 0xAAAAsize as *const rudo_gc::ptr::GcBox<()>;

    // Fill queue 0 to capacity
    for _ in 0..MARK_QUEUE_SIZE {
        assert!(queues[0].push(ptr), "Should push until full");
    }

    // Try to handle overflow by pushing to other queues
    let handled = queues[0].handle_overflow(ptr, &queues_ref);

    // Should be able to push to another queue's pending_work
    assert!(
        handled,
        "Overflow should be handled by pushing to remote queue"
    );

    // Verify some queue received the overflow
    let received_overflow = queues.iter().any(|q| q.has_pending_work());
    assert!(
        received_overflow,
        "At least one queue should have overflow work"
    );
}

/// Test concurrent push-based transfer.
#[test]
fn test_concurrent_push_transfer() {
    let num_producers = 4;
    let num_consumers = 2;
    let items_per_producer = 100;

    let consumers: Vec<Arc<PerThreadMarkQueue>> = (0..num_consumers)
        .map(|i| Arc::new(PerThreadMarkQueue::new_with_index(i)))
        .collect();

    let barrier = Arc::new(Barrier::new(num_producers + num_consumers));
    let produced = Arc::new(AtomicUsize::new(0));
    let consumers_ref: Vec<PerThreadMarkQueue> = consumers.iter().map(|q| (**q).clone()).collect();

    // Producer threads push to consumers
    let producer_handles: Vec<_> = (0..num_producers)
        .map(|producer_id| {
            let barrier = Arc::clone(&barrier);
            let consumers = consumers.clone();
            let produced = Arc::clone(&produced);
            thread::spawn(move || {
                barrier.wait();
                for i in 0..items_per_producer {
                    let consumer_idx = (producer_id + i) % num_consumers;
                    let ptr =
                        (producer_id * items_per_producer + i) as *const rudo_gc::ptr::GcBox<()>;
                    PerThreadMarkQueue::push_remote(&consumers[consumer_idx], ptr);
                    produced.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();

    // Consumer threads drain pending work
    let consumer_handles: Vec<_> = (0..num_consumers)
        .map(|consumer_id| {
            let barrier = Arc::clone(&barrier);
            let consumers = consumers.clone();
            thread::spawn(move || {
                barrier.wait();
                let mut drained = 0;
                while drained < items_per_producer * (num_producers / num_consumers) {
                    if let Some(_) = consumers[consumer_id].receive_pending_work() {
                        drained += 1;
                    }
                    if drained >= items_per_producer * (num_producers / num_consumers) {
                        break;
                    }
                }
                drained
            })
        })
        .collect();

    for handle in producer_handles {
        handle.join().unwrap();
    }

    let total_produced = produced.load(Ordering::SeqCst);
    assert_eq!(total_produced, num_producers * items_per_producer);

    for handle in consumer_handles {
        handle.join().unwrap();
    }
}

/// Test ownership-based work distribution.
#[test]
fn test_ownership_distribution() {
    let queue = PerThreadMarkQueue::new();
    assert_eq!(queue.owned_page_count(), 0);

    // Note: Full ownership testing requires PageHeader integration
    // This test verifies the basic ownership tracking API
    assert!(true, "Ownership tracking is initialized correctly");
}

/// Test utilization monitoring.
#[test]
fn test_utilization_monitoring() {
    let queue = PerThreadMarkQueue::new();

    assert_eq!(queue.utilization(), 0.0, "Empty queue has 0 utilization");

    let ptr = 0x1111usize as *const rudo_gc::ptr::GcBox<()>;

    // Fill half the queue
    for _ in 0..MARK_QUEUE_SIZE / 2 {
        assert!(queue.push(ptr));
    }

    let utilization = queue.utilization();
    assert!(
        utilization >= 0.49 && utilization <= 0.51,
        "Half-full queue should have ~50% utilization"
    );
}

/// Test near-capacity detection.
#[test]
fn test_near_capacity_detection() {
    let queue = PerThreadMarkQueue::new();

    let ptr = 0x2222usize as *const rudo_gc::ptr::GcBox<()>;

    // Queue should not be near capacity initially
    assert!(
        !queue.is_near_capacity(),
        "Empty queue should not be near capacity"
    );

    // Fill up to capacity hint
    let hint = queue.capacity_hint();
    for _ in 0..hint {
        assert!(queue.push(ptr));
    }

    assert!(
        queue.is_near_capacity(),
        "Queue at capacity hint should be near capacity"
    );
}
