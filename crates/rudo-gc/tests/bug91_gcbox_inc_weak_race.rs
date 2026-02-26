//! Regression test for Bug 91: `GcBox::inc_weak` load+store race condition.
//!
//! `inc_weak` used load+store instead of atomic `fetch_update`, causing lost
//! updates when multiple threads concurrently create Weak references.
//!
//! See: docs/issues/2026-02-24_ISSUE_bug91_gcbox_inc_weak_race_condition.md

use rudo_gc::{collect_full, Gc, Trace};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_concurrent_inc_weak_no_lost_updates() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);

    let num_threads = 4;
    let clones_per_thread = 500;
    let barrier = Arc::new(Barrier::new(num_threads));
    let weak_refs_created = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let w = weak.clone();
            let b = Arc::clone(&barrier);
            let counter = Arc::clone(&weak_refs_created);
            thread::spawn(move || {
                b.wait();
                let mut refs = vec![];
                for _ in 0..clones_per_thread {
                    refs.push(w.clone());
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                refs
            })
        })
        .collect();

    let all_refs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    collect_full();

    let total_weak_refs: usize = all_refs.iter().map(Vec::len).sum();
    assert_eq!(total_weak_refs, num_threads * clones_per_thread);
    assert_eq!(
        weak_refs_created.load(Ordering::Relaxed),
        num_threads * clones_per_thread
    );

    for refs in &all_refs {
        for w in refs {
            if let Some(g) = w.upgrade() {
                assert_eq!(g.value, 42);
            }
        }
    }
}
