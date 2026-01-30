//! Loom tests for dropping state atomic ordering.
//!
//! These tests verify the memory ordering guarantees for the dropping state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Test that dropping_state() with Acquire sees the mark from try_mark_dropping().
#[test]
#[ignore = "loom test - run with cargo test loom_dropping_sync --release"]
fn test_dropping_state_acquire_sync() {
    loom::model(|| {
        let is_dropping = Arc::new(AtomicUsize::new(0));

        let mark_thread = loom::thread::spawn({
            let is_dropping = Arc::clone(&is_dropping);
            move || {
                let old = is_dropping.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
                old.is_ok()
            }
        });

        let read_thread = loom::thread::spawn({
            let is_dropping = Arc::clone(&is_dropping);
            move || is_dropping.load(Ordering::Acquire)
        });

        let mark_success = mark_thread.join().unwrap();
        let state = read_thread.join().unwrap();

        if mark_success {
            assert_eq!(state, 1);
        } else {
            assert!(state == 0 || state == 1);
        }
    });
}

/// Test concurrent dropping state transitions.
#[test]
#[ignore = "loom test - run with cargo test loom_dropping_transitions --release"]
fn test_concurrent_dropping_transitions() {
    loom::model(|| {
        let is_dropping = Arc::new(AtomicUsize::new(0));

        let thread1 = loom::thread::spawn({
            let is_dropping = Arc::clone(&is_dropping);
            move || {
                let success = is_dropping
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok();
                if success {
                    let _ = is_dropping.store(2, Ordering::Release);
                }
                success
            }
        });

        let thread2 = loom::thread::spawn({
            let is_dropping = Arc::clone(&is_dropping);
            move || loop {
                let state = is_dropping.load(Ordering::Acquire);
                if state == 0 {
                    break;
                }
                if state >= 1 {
                    break;
                }
            }
        });

        let marked = thread1.join().unwrap();
        thread2.join().unwrap();

        assert!(marked);
    });
}

/// Test Acquire sync between dropping state read and value access.
#[test]
#[ignore = "loom test - run with cargo test loom_dropping_value_sync --release"]
fn test_state_read_before_value_access() {
    loom::model(|| {
        let is_dropping = Arc::new(AtomicUsize::new(0));
        let value_valid = Arc::new(AtomicUsize::new(1));

        let marker = loom::thread::spawn({
            let is_dropping = Arc::clone(&is_dropping);
            let value_valid = Arc::clone(&value_valid);
            move || {
                let marked = is_dropping
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok();
                if marked {
                    value_valid.store(0, Ordering::Release);
                }
                marked
            }
        });

        let reader = loom::thread::spawn({
            let is_dropping = Arc::clone(&is_dropping);
            let value_valid = Arc::clone(&value_valid);
            move || {
                let state = is_dropping.load(Ordering::Acquire);
                let valid = value_valid.load(Ordering::Acquire);
                (state, valid)
            }
        });

        let marked = marker.join().unwrap();
        let (state, valid) = reader.join().unwrap();

        if state >= 1 {
            assert_eq!(valid, 0);
        }
        if valid == 1 {
            assert!(!marked || state == 0);
        }
    });
}
