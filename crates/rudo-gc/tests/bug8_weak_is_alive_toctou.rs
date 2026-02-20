//! Test for Bug 8: `Weak::is_alive()` TOCTOU race
//!
//! Between ptr.load and `*ptr.as_ptr()` in `is_alive()`, GC may run and reclaim the object,
//! causing use-after-free when `has_dead_flag()` dereferences.
//!
//! See: docs/issues/2026-02-19_ISSUE_bug8_weak_is_alive_toctou.md

use rudo_gc::{collect_full, Gc, Trace};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_weak_is_alive_toctou() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);

    let is_alive_called = Arc::new(AtomicBool::new(false));
    let is_alive_called_clone = is_alive_called.clone();

    let handle = thread::spawn(move || {
        while !is_alive_called_clone.load(Ordering::Relaxed) {
            thread::yield_now();
        }
        let alive = weak.is_alive();
        std::hint::black_box(alive);
    });

    drop(gc);
    collect_full();

    is_alive_called.store(true, Ordering::Relaxed);

    // If bug exists: is_alive() may UAF -> panic, segfault, or UB
    let result = handle.join();
    assert!(
        result.is_ok(),
        "is_alive() should not cause thread to panic"
    );
}
