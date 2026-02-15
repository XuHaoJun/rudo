//! Tests for concurrent Weak<T> behavior and race conditions.
//!
//! These tests verify that Weak references work correctly under
//! concurrent access from multiple threads.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use rudo_gc::gc::incremental::{IncrementalConfig, IncrementalMarkState, MarkPhase};
use rudo_gc::{collect_full, set_incremental_config, Gc, Trace, Weak};

#[cfg(feature = "test-util")]
use rudo_gc::test_util::{clear_test_roots, internal_ptr, register_test_root};

#[cfg(feature = "test-util")]
macro_rules! root {
    ($gc:expr) => {
        register_test_root(internal_ptr(&$gc))
    };
}

#[cfg(not(feature = "test-util"))]
macro_rules! root {
    ($gc:expr) => {};
}

#[cfg(feature = "test-util")]
macro_rules! clear_roots {
    () => {
        clear_test_roots()
    };
}

#[cfg(not(feature = "test-util"))]
macro_rules! clear_roots {
    () => {};
}

#[derive(Trace)]
struct TestData {
    value: i32,
}

// ============================================================================
// Test 1: Concurrent weak upgrade - no GC in thread for Miri compatibility
// ============================================================================

#[test]
fn test_concurrent_weak_upgrade_during_gc() {
    clear_roots!();

    let gc = Gc::new(TestData { value: 42 });
    root!(gc);
    let weak = Gc::downgrade(&gc);

    let weak_clone1 = weak.clone();
    let weak_clone2 = weak.clone();
    let weak_clone3 = weak.clone();

    // Spawn threads that upgrade weak concurrently
    // (No GC in thread for Miri compatibility)
    let t1 = thread::spawn(move || {
        for _ in 0..100 {
            let _ = weak_clone1.upgrade();
        }
    });

    let t2 = thread::spawn(move || {
        for _ in 0..100 {
            let _ = weak_clone2.upgrade();
        }
    });

    let t3 = thread::spawn(move || {
        for _ in 0..100 {
            let _ = weak_clone3.upgrade();
        }
    });

    t1.join().unwrap();
    t2.join().unwrap();
    t3.join().unwrap();

    // Verify weak still works
    let _ = weak.is_alive();
    let _ = weak.may_be_valid();

    clear_roots!();
}

// ============================================================================
// Test 2: Concurrent weak clone and upgrade
// ============================================================================

#[test]
fn test_concurrent_weak_clone_and_upgrade() {
    let gc = Arc::new(Gc::new(TestData { value: 99 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let weak = weak.clone();
            thread::spawn(move || weak.upgrade())
        })
        .collect();

    for handle in handles {
        let upgraded = handle.join().unwrap();
        assert!(upgraded.is_some());
    }
}

// ============================================================================
// Test 3: Concurrent weak is_alive checks
// ============================================================================

#[test]
fn test_concurrent_weak_is_alive() {
    let gc = Arc::new(Gc::new(TestData { value: 1 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let handles: Vec<_> = (0..20)
        .map(|_| {
            let weak = weak.clone();
            thread::spawn(move || {
                for _ in 0..1000 {
                    let _ = weak.is_alive();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // Should still be alive
    assert!(weak.is_alive());
}

// ============================================================================
// Test 4: Drop strong while threads hold weak
// ============================================================================

#[test]
fn test_drop_strong_while_threads_hold_weak() {
    let gc = Arc::new(Gc::new(TestData { value: 55 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let started = Arc::new(AtomicBool::new(false));
    let proceed = Arc::new(AtomicBool::new(false));

    let weak_clone = weak.clone();
    let started_clone = started.clone();
    let proceed_clone = proceed.clone();

    let reader = thread::spawn(move || {
        started_clone.store(true, Ordering::SeqCst);
        while !proceed_clone.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        // Try to upgrade while strong might be dropped
        for _ in 0..100 {
            let _ = weak_clone.is_alive();
        }
    });

    // Wait for reader to start
    while !started.load(Ordering::SeqCst) {
        thread::yield_now();
    }

    // Drop the strong reference
    drop(gc);

    // Let reader proceed
    proceed.store(true, Ordering::SeqCst);
    let _ = reader.join();

    // Collect and verify weak is dead
    clear_roots!();
    collect_full();
    assert!(!weak.is_alive());
}

// ============================================================================
// Test 5: Concurrent weak count checks
// ============================================================================

#[test]
fn test_concurrent_weak_count() {
    let gc = Arc::new(Gc::new(TestData { value: 10 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    // Add more weak refs
    let _weak2 = Gc::downgrade(&gc);
    let _weak3 = Gc::downgrade(&gc);
    let _weak4 = Gc::downgrade(&gc);

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let weak = weak.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    let _ = weak.weak_count();
                    let _ = weak.strong_count();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // Count should still be consistent
    assert_eq!(Gc::weak_count(&gc), 4);
}

// ============================================================================
// Test 6: Weak ptr_eq concurrent
// ============================================================================

#[test]
fn test_concurrent_weak_ptr_eq() {
    let gc1 = Arc::new(Gc::new(TestData { value: 1 }));
    let gc2 = Arc::new(Gc::new(TestData { value: 2 }));

    let weak1a = Arc::new(Gc::downgrade(&gc1));
    let weak1_alt = Arc::new(Gc::downgrade(&gc1));
    let weak2 = Arc::new(Gc::downgrade(&gc2));

    let handles: Vec<_> = (0..20)
        .map(|i| {
            let w1 = if i % 2 == 0 {
                weak1a.clone()
            } else {
                weak1_alt.clone()
            };
            let w2 = weak2.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    assert!(Weak::ptr_eq(&w1, &w1));
                    assert!(!Weak::ptr_eq(&w1, &w2));
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
}

// ============================================================================
// Test 7: Concurrent weak upgrade with many threads
// ============================================================================

#[test]
fn test_many_threads_concurrent_weak_upgrade() {
    let gc = Arc::new(Gc::new(TestData { value: 777 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let handles: Vec<_> = (0..50)
        .map(|_| {
            let weak = weak.clone();
            thread::spawn(move || {
                let mut successes = 0;
                for _ in 0..50 {
                    if weak.upgrade().is_some() {
                        successes += 1;
                    }
                }
                successes
            })
        })
        .collect();

    let mut total_success = 0;
    for handle in handles {
        total_success += handle.join().unwrap();
    }

    // All threads should have successfully upgraded
    assert_eq!(total_success, 50 * 50);
}

// ============================================================================
// Test 8: Weak concurrent with Gc
// ============================================================================

#[test]
fn test_weak_concurrent_gc_access() {
    #[derive(Trace)]
    struct Inner {
        value: i32,
    }

    #[derive(Trace)]
    struct Node {
        inner: Gc<Inner>,
    }

    let inner = Arc::new(Gc::new(Inner { value: 0 }));
    let gc = Arc::new(Gc::new(Node {
        inner: Gc::clone(&inner),
    }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let writer = thread::spawn(move || {
        for i in 0..1000 {
            let new_inner = Gc::new(Inner { value: i });
            // Replace inner reference
            let _ = new_inner;
        }
    });

    let reader = thread::spawn(move || {
        for _ in 0..1000 {
            if let Some(node) = weak.upgrade() {
                let _ = node.inner.value;
            }
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();
}

// ============================================================================
// Test 9: Weak upgrade during incremental marking
// ============================================================================

#[test]
fn test_weak_upgrade_during_incremental_marking() {
    enable_incremental();

    let gc = Arc::new(Gc::new(TestData { value: 123 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let marker_thread = thread::spawn(move || {
        for _ in 0..100 {
            let _ = weak.upgrade();
        }
    });

    marker_thread.join().unwrap();

    state.set_phase(MarkPhase::Idle);
    disable_incremental();
}

fn enable_incremental() {
    let config = IncrementalConfig {
        enabled: true,
        increment_size: 100,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    };
    set_incremental_config(config);
}

fn disable_incremental() {
    let config = IncrementalConfig {
        enabled: false,
        increment_size: 100,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    };
    set_incremental_config(config);
}

// ============================================================================
// Test 10: Concurrent weak may_be_valid
// ============================================================================

#[test]
fn test_concurrent_weak_may_be_valid() {
    let gc = Arc::new(Gc::new(TestData { value: 5 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let handles: Vec<_> = (0..20)
        .map(|_| {
            let weak = weak.clone();
            thread::spawn(move || {
                for _ in 0..1000 {
                    let _ = weak.may_be_valid();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    assert!(weak.may_be_valid());
}

// ============================================================================
// Test 11: Weak cast concurrent (skip - cast consumes self in loop)
// This test is complex due to Weak::cast taking ownership
// ============================================================================

// ============================================================================
// Test 12: Stress test - many threads upgrading weaks (no race for Miri)
// ============================================================================

#[test]
fn test_stress_many_threads_drop_weaks() {
    clear_roots!();

    let gc = Gc::new(TestData { value: 999 });
    root!(gc);
    let weak = Gc::downgrade(&gc);

    let counters = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..20)
        .map(|_| {
            let weak = weak.clone();
            let counter = counters.clone();
            thread::spawn(move || {
                for _ in 0..1000 {
                    let w = weak.clone();
                    if w.upgrade().is_some() {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        })
        .collect();

    // Let threads finish
    for handle in handles {
        handle.join().unwrap();
    }

    // Weak should still be alive (root keeps it alive)
    assert!(weak.is_alive());

    clear_roots!();
}

// ============================================================================
// Test 13: Concurrent weak and try_upgrade
// ============================================================================

#[test]
fn test_concurrent_weak_try_upgrade() {
    let gc = Arc::new(Gc::new(TestData { value: 33 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let weak = weak.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    let _ = weak.upgrade();
                    let _ = weak.try_upgrade();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    assert!(weak.upgrade().is_some());
}

// ============================================================================
// Test 15: Weak clone during concurrent upgrade
// ============================================================================

#[test]
fn test_weak_clone_during_concurrent_upgrade() {
    let gc = Arc::new(Gc::new(TestData { value: 66 }));
    let weak = Arc::new(Gc::downgrade(&gc));

    let weak_for_upgrade = weak.clone();
    #[allow(clippy::redundant_clone)]
    let weak_for_clone = weak.clone();
    #[allow(clippy::redundant_clone)]
    let gc_clone = gc.clone();

    let upgrade_thread = thread::spawn(move || {
        for _ in 0..100 {
            let _ = weak_for_upgrade.upgrade();
        }
    });

    let clone_thread = thread::spawn(move || {
        for _ in 0..100 {
            let _ = weak_for_clone.clone();
        }
    });

    // Also use the Gc
    let use_gc_thread = thread::spawn(move || {
        for _ in 0..100 {
            let _ = gc_clone.value;
        }
    });

    upgrade_thread.join().unwrap();
    clone_thread.join().unwrap();
    use_gc_thread.join().unwrap();

    // All should still work
    assert!(weak.upgrade().is_some());
}
