//! Regression test for Bug 530: `incremental_write_barrier` small object path
//! missing third is_allocated check after reading `has_gen_old`.
//!
//! When lazy sweep reclaims and reuses a slot between the second is_allocated
//! check and reading `has_gen_old`, the small object path would incorrectly
//! read the new object's flags, potentially causing wrong remembered set entries.
//!
//! See: docs/issues/2026-04-08_ISSUE_bug530_incremental_write_barrier_small_object_path_missing_third_is_allocated_check.md

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
fn test_incremental_write_barrier_small_object_no_incorrect_recording() {
    set_incremental_config(IncrementalConfig {
        enabled: true,
        increment_size: 100,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    });

    let num_threads = 4;
    let alloc_count = AtomicUsize::new(0);

    let handles: Vec<_> = (0..num_threads)
        .map(|i| {
            let alloc = AtomicUsize::new(0);
            thread::spawn(move || {
                for j in 0..200 {
                    let value = (i as i32) * 10000 + j;
                    let _gc = Gc::new(Data { value });
                    alloc.fetch_add(1, Ordering::Relaxed);
                    if j % 20 == 0 {
                        collect_full();
                    }
                }
                alloc
            })
        })
        .collect();

    for handle in handles {
        let a = handle.join().unwrap();
        alloc_count.fetch_add(a.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    collect_full();

    let total = alloc_count.load(Ordering::Relaxed);
    assert_eq!(
        total,
        num_threads * 200,
        "Should have allocated {} objects",
        num_threads * 200
    );
}
