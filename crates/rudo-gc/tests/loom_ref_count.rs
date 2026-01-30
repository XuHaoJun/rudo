//! Loom tests for reference count atomic ordering.
//!
//! These tests verify the memory ordering guarantees for reference counting.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Test that Acquire load sees the complete effect of a decrement.
#[test]
#[ignore = "loom test - run with cargo test loom_ref_count_acquire --release"]
fn test_acquire_load_sees_decrement() {
    loom::model(|| {
        let ref_count = Arc::new(AtomicUsize::new(2));

        let dec_thread = loom::thread::spawn({
            let ref_count = Arc::clone(&ref_count);
            move || {
                ref_count.fetch_sub(1, Ordering::AcqRel);
            }
        });

        let read_thread = loom::thread::spawn({
            let ref_count = Arc::clone(&ref_count);
            move || ref_count.load(Ordering::Acquire)
        });

        dec_thread.join().unwrap();
        let value = read_thread.join().unwrap();

        assert!(value == 1 || value == 2);
    });
}

/// Test concurrent increment and decrement operations.
#[test]
#[ignore = "loom test - run with cargo test loom_ref_count_concurrent --release"]
fn test_concurrent_inc_dec() {
    loom::model(|| {
        let ref_count = Arc::new(AtomicUsize::new(1));

        let inc_thread = loom::thread::spawn({
            let ref_count = Arc::clone(&ref_count);
            move || {
                ref_count
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                        if c == usize::MAX {
                            None
                        } else {
                            Some(c.saturating_add(1))
                        }
                    })
                    .ok();
            }
        });

        let dec_thread = loom::thread::spawn({
            let ref_count = Arc::clone(&ref_count);
            move || {
                ref_count.fetch_sub(1, Ordering::AcqRel);
            }
        });

        inc_thread.join().unwrap();
        dec_thread.join().unwrap();

        let final_count = ref_count.load(Ordering::Acquire);
        assert!(final_count == 1);
    });
}

/// Test that zero read is properly synchronized.
#[test]
#[ignore = "loom test - run with cargo test loom_ref_count_zero --release"]
fn test_zero_read_synchronization() {
    loom::model(|| {
        let ref_count = Arc::new(AtomicUsize::new(1));
        let dropped = Arc::new(AtomicUsize::new(0));

        let drop_thread = loom::thread::spawn({
            let ref_count = Arc::clone(&ref_count);
            let dropped = Arc::clone(&dropped);
            move || {
                let old = ref_count.fetch_sub(1, Ordering::AcqRel);
                if old == 1 {
                    dropped.store(1, Ordering::Release);
                }
                old
            }
        });

        let read_thread = loom::thread::spawn({
            let ref_count = Arc::clone(&ref_count);
            let dropped = Arc::clone(&dropped);
            move || {
                let count = ref_count.load(Ordering::Acquire);
                let is_dropped = dropped.load(Ordering::Acquire);
                (count, is_dropped)
            }
        });

        let _drop_old = drop_thread.join().unwrap();
        let (count, is_dropped) = read_thread.join().unwrap();

        if count == 0 {
            assert_eq!(is_dropped, 1);
        }
    });
}
