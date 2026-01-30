#![allow(clippy::doc_markdown)]
#![allow(clippy::needless_pass_by_value)]

//! Integration test for multi-runtime support.
//!
//! This test verifies that `GcRootSet` correctly tracks roots across
//! multiple tokio runtimes running concurrently.

use rudo_gc::tokio::{spawn, GcRootSet, GcTokioExt};
use rudo_gc::{Gc, Trace};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

static RUNTIME1_GC_COUNT: AtomicUsize = AtomicUsize::new(0);
static RUNTIME2_GC_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Trace)]
struct SharedData {
    value: Gc<i32>,
}

fn runtime1_worker(shared: Arc<SharedData>) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let gc_clone = Gc::clone(&shared.value);
        let _guard = gc_clone.root_guard();

        RUNTIME1_GC_COUNT.fetch_add(1, Ordering::SeqCst);

        let handle = spawn(async move { *gc_clone });

        let result = handle.await;
        assert_eq!(result, 42);

        RUNTIME1_GC_COUNT.fetch_add(1, Ordering::SeqCst);
    });
}

fn runtime2_worker(shared: Arc<SharedData>) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap();

    rt.block_on(async {
        let gc_clone = Gc::clone(&shared.value);
        let _guard = gc_clone.root_guard();

        RUNTIME2_GC_COUNT.fetch_add(1, Ordering::SeqCst);

        for i in 0..5 {
            let val = Gc::clone(&gc_clone);
            let result = spawn(async move { *val + i }).await;
            assert!(result >= 42 && result <= 46);
        }

        RUNTIME2_GC_COUNT.fetch_add(1, Ordering::SeqCst);
    });
}

#[test]
fn test_multi_runtime_root_tracking() {
    let shared = Arc::new(SharedData { value: Gc::new(42) });

    let shared1 = Arc::clone(&shared);
    let handle1 = thread::spawn(move || {
        runtime1_worker(shared1);
    });

    let shared2 = Arc::clone(&shared);
    let handle2 = thread::spawn(move || {
        runtime2_worker(shared2);
    });

    handle1.join().unwrap();
    handle2.join().unwrap();

    assert_eq!(RUNTIME1_GC_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(RUNTIME2_GC_COUNT.load(Ordering::SeqCst), 2);
}

#[test]
fn test_single_runtime_multiple_threads() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
        .unwrap();

    let counter = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(4));

    let mut handles = Vec::new();

    for _ in 0..4 {
        let counter = Arc::clone(&counter);
        let barrier = Arc::clone(&barrier);

        handles.push(rt.spawn(async move {
            let gc = Gc::new(100);
            let _guard = gc.root_guard();

            barrier.wait().await;

            counter.fetch_add(*gc, Ordering::SeqCst);
            counter.fetch_add(1, Ordering::SeqCst);
        }));
    }

    rt.block_on(async {
        for handle in handles {
            handle.await.unwrap();
        }
    });

    assert_eq!(counter.load(Ordering::SeqCst), 404);
}

#[test]
fn test_runtime_lifecycle_with_gc_roots() {
    for _ in 0..3 {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let gc = Gc::new(999);
            let _guard = gc.root_guard();

            assert_eq!(*gc, 999);
        });

        rt.shutdown_background();
    }
}

#[test]
fn test_gcrootset_singleton_across_runtimes() {
    let set1 = GcRootSet::global();
    let set2 = GcRootSet::global();

    assert!(std::ptr::eq(set1, set2));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let set3 = GcRootSet::global();
        assert!(std::ptr::eq(set1, set3));
    });

    let rt2 = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt2.block_on(async {
        let set4 = GcRootSet::global();
        assert!(std::ptr::eq(set1, set4));
    });
}

#[test]
fn test_dirty_flag_behavior() {
    let set = GcRootSet::global();
    set.clear();

    assert!(set.is_dirty());

    let test_ptr = 0x12345;
    set.register(test_ptr);
    assert!(set.is_dirty());

    let _snapshot = set.snapshot();
    assert!(!set.is_dirty());

    set.unregister(test_ptr);
    assert!(set.is_dirty());
}
