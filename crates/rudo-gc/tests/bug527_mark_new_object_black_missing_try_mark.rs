//! Regression test for Bug 527: `mark_new_object_black` missing `try_mark` + generation check
//!
//! When lazy sweep reclaims a slot between `is_marked` check and `set_mark`,
//! `mark_new_object_black` may incorrectly mark the new object that took the slot.
//!
//! See: docs/issues/2026-04-07_ISSUE_bug527_mark_new_object_black_missing_try_mark.md

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use rudo_gc::gc::incremental::IncrementalConfig;
use rudo_gc::{collect_full, set_incremental_config, Gc, Trace};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_mark_new_object_black_no_incorrect_marking() {
    set_incremental_config(IncrementalConfig {
        enabled: true,
        increment_size: 100,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    });

    let num_threads = 4;
    let marked_count = AtomicUsize::new(0);

    let handles: Vec<_> = (0..num_threads)
        .map(|i| {
            let marked = AtomicUsize::new(0);
            thread::spawn(move || {
                for j in 0..100 {
                    let value = (i as i32) * 1000 + j;
                    let _gc = Gc::new(Data { value });
                    if j % 10 == 0 {
                        collect_full();
                    }
                }
                marked.fetch_add(100, Ordering::Relaxed);
                marked
            })
        })
        .collect();

    for handle in handles {
        let m = handle.join().unwrap();
        marked_count.fetch_add(m.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    collect_full();

    let total = marked_count.load(Ordering::Relaxed);
    assert_eq!(
        total,
        num_threads * 100,
        "Should have marked {} objects",
        num_threads * 100
    );
}
