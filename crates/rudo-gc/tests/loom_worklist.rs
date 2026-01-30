//! Loom tests for work-stealing queue atomic ordering.
//!
//! These tests verify the memory ordering guarantees for the StealQueue
//! implementation.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use rudo_gc::StealQueue;

const QUEUE_SIZE: usize = 4;

/// Test that after a push, a concurrent steal can see the data.
#[test]
#[ignore = "loom test - run with cargo test loom_worklist_push --release"]
fn test_push_then_steal_sees_data() {
    loom::model(|| {
        let queue: Arc<StealQueue<i32, QUEUE_SIZE>> = Arc::new(StealQueue::new());
        let bottom = Arc::new(AtomicUsize::new(0));

        let push_thread = loom::thread::spawn({
            let queue = Arc::clone(&queue);
            let bottom = Arc::clone(&bottom);
            move || queue.push(&bottom, 42)
        });

        let steal_thread = loom::thread::spawn({
            let queue = Arc::clone(&queue);
            let bottom = Arc::clone(&bottom);
            move || queue.steal(&bottom)
        });

        let pushed_result = push_thread.join().unwrap();
        let steal_result = steal_thread.join().unwrap();

        if pushed_result {
            assert!(steal_result.is_none() || steal_result == Some(42));
        }
    });
}

/// Test concurrent push and steal operations.
#[test]
#[ignore = "loom test - run with cargo test loom_worklist_concurrent --release"]
fn test_concurrent_push_steal() {
    loom::model(|| {
        let queue: Arc<StealQueue<i32, QUEUE_SIZE>> = Arc::new(StealQueue::new());
        let bottom = Arc::new(AtomicUsize::new(0));

        let push_thread = loom::thread::spawn({
            let queue = Arc::clone(&queue);
            let bottom = Arc::clone(&bottom);
            move || {
                queue.push(&bottom, 0);
                queue.push(&bottom, 1);
            }
        });

        let steal_thread = loom::thread::spawn({
            let queue = Arc::clone(&queue);
            let bottom = Arc::clone(&bottom);
            move || queue.steal(&bottom)
        });

        push_thread.join().unwrap();
        let stolen = steal_thread.join().unwrap();

        assert!(stolen.is_none() || stolen == Some(0) || stolen == Some(1));
    });
}

/// Test that empty queue returns None for both pop and steal.
#[test]
#[ignore = "loom test - run with cargo test loom_worklist_empty --release"]
fn test_empty_queue_operations() {
    loom::model(|| {
        let queue: Arc<StealQueue<i32, QUEUE_SIZE>> = Arc::new(StealQueue::new());
        let bottom = Arc::new(AtomicUsize::new(0));

        let pop_thread = loom::thread::spawn({
            let queue = Arc::clone(&queue);
            let bottom = Arc::clone(&bottom);
            move || queue.pop(&bottom)
        });

        let steal_thread = loom::thread::spawn({
            let queue = Arc::clone(&queue);
            let bottom = Arc::clone(&bottom);
            move || queue.steal(&bottom)
        });

        let pop_result = pop_thread.join().unwrap();
        let steal_result = steal_thread.join().unwrap();

        assert!(pop_result.is_none());
        assert!(steal_result.is_none());
    });
}
