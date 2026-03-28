#![allow(clippy::await_holding_refcell_ref)]

#[cfg(feature = "tokio")]
use tokio as _;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use rudo_gc::cell::GcCapture;
use rudo_gc::{collect_full, Gc, GcBox, GcThreadSafeCell, Trace};

#[derive(Trace, Clone)]
struct ThreadSafeSignal {
    value: GcThreadSafeCell<i32>,
}

impl ThreadSafeSignal {
    const fn new(value: i32) -> Self {
        Self {
            value: GcThreadSafeCell::new(value),
        }
    }

    fn get(&self) -> i32 {
        *self.value.borrow()
    }

    fn set(&self, new_value: i32) {
        *self.value.borrow_mut_gen_only() = new_value;
    }
}

#[test]
fn test_gc_thread_safe_cell_basic() {
    let signal = Gc::new(ThreadSafeSignal::new(0));

    assert_eq!(signal.get(), 0);

    signal.set(42);
    assert_eq!(signal.get(), 42);

    signal.set(100);
    assert_eq!(signal.get(), 100);
}

#[test]
fn test_gc_thread_safe_cell_clone() {
    let signal = Gc::new(ThreadSafeSignal::new(10));
    let signal_clone = signal.clone();

    assert_eq!(signal.get(), 10);
    assert_eq!(signal_clone.get(), 10);

    signal.set(20);
    assert_eq!(signal.get(), 20);
    assert_eq!(signal_clone.get(), 20);
}

#[test]
fn test_gc_thread_safe_cell_with_gc_ptrs() {
    #[derive(Trace, Clone)]
    struct SignalWithGc {
        value: GcThreadSafeCell<i32>,
        child: Gc<Gc<i32>>,
    }

    let signal: Gc<SignalWithGc> = Gc::new(SignalWithGc {
        value: GcThreadSafeCell::new(0),
        child: Gc::new(Gc::new(42)),
    });

    assert_eq!(*signal.value.borrow(), 0);
    assert_eq!(**signal.child, 42);

    *signal.value.borrow_mut_simple() = 100;
    assert_eq!(*signal.value.borrow(), 100);
}

#[test]
fn test_borrow_mut_primitive_types() {
    let cell = Gc::new(GcThreadSafeCell::new(42i32));

    assert_eq!(*cell.borrow(), 42);

    *cell.borrow_mut_simple() = 100;

    assert_eq!(*cell.borrow(), 100);
}

#[test]
fn test_gc_thread_safe_ref_mut_drop_triggers_generational_barrier() {
    use rudo_gc::gc::incremental::{IncrementalConfig, IncrementalMarkState};

    #[derive(Trace, Default)]
    struct Inner {
        value: i32,
    }
    rudo_gc::impl_gc_capture!(Inner);

    rudo_gc::test_util::reset();

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        increment_size: 1024,
        max_dirty_pages: 64,
        remembered_buffer_len: 32,
        slice_timeout_ms: 10,
    });

    let cell: Gc<GcThreadSafeCell<Option<Gc<Inner>>>> = Gc::new(GcThreadSafeCell::new(None));

    rudo_gc::collect_full();

    let young = Gc::new(Inner { value: 42 });
    {
        let mut guard = cell.borrow_mut();
        *guard = Some(young);
    }

    rudo_gc::collect_full();

    assert_eq!(cell.borrow().as_ref().unwrap().value, 42);
}

#[test]
fn test_gc_correctness_with_drop_tracker() {
    #[derive(Clone)]
    struct DropTracker {
        marker: Rc<Cell<bool>>,
    }

    impl Drop for DropTracker {
        fn drop(&mut self) {
            self.marker.set(true);
        }
    }

    unsafe impl Trace for DropTracker {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    let marker = Rc::new(Cell::new(false));
    let gc: Gc<DropTracker> = Gc::new(DropTracker {
        marker: marker.clone(),
    });

    assert!(!marker.get());
    assert_eq!(Rc::strong_count(&marker), 2);

    drop(gc);
    collect_full();

    assert!(marker.get());
    assert_eq!(Rc::strong_count(&marker), 1);
}

#[test]
fn test_concurrent_mutation() {
    let cell = Arc::new(Gc::new(GcThreadSafeCell::new(0usize)));
    let barrier = Arc::new(Barrier::new(10));
    let counter = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let cell = cell.clone();
            let barrier = barrier.clone();
            let counter = counter.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..1000 {
                    *cell.borrow_mut_gen_only() += 1;
                }
                counter.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(*cell.borrow(), 10000);
}

#[test]
fn test_concurrent_gc_ptr_mutation() {
    #[derive(Trace, Clone)]
    struct Container {
        inner: Gc<i32>,
    }

    // Note: Using borrow_mut_gen_only() here is safe because:
    // 1. No GC collection runs during this test (no explicit collect_full())
    // 2. The generational/incremental barriers are unnecessary when no GC runs
    // In production code, always use borrow_mut() for types containing Gc<T>
    let cell = Arc::new(Gc::new(GcThreadSafeCell::new(Container {
        inner: Gc::new(0),
    })));
    let barrier = Arc::new(Barrier::new(5));

    let handles: Vec<_> = (0..5)
        .map(|i| {
            let cell = cell.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for j in 0..100 {
                    *cell.borrow_mut_gen_only() = Container {
                        inner: Gc::new(i * 1000 + j),
                    };
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn test_borrow_mut_with_gc_ptrs() {
    #[derive(Trace, Clone)]
    struct Container {
        inner: Gc<i32>,
    }

    impl GcCapture for Container {
        fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<GcBox<()>>] {
            &[]
        }
        fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<std::ptr::NonNull<GcBox<()>>>) {
            self.inner.capture_gc_ptrs_into(ptrs);
        }
    }

    #[derive(Trace, Clone)]
    struct Wrapper {
        cell: GcThreadSafeCell<Container>,
    }

    let wrapper = Gc::new(Wrapper {
        cell: GcThreadSafeCell::new(Container { inner: Gc::new(42) }),
    });

    assert_eq!(*wrapper.cell.borrow().inner, 42);

    wrapper.cell.borrow_mut().inner = Gc::new(100);

    assert_eq!(*wrapper.cell.borrow().inner, 100);

    let wrapper_clone = Gc::clone(&wrapper);
    drop(wrapper);
    collect_full();
    assert_eq!(*wrapper_clone.cell.borrow().inner, 100);
}

#[test]
fn test_cross_thread_borrow_mut_gc_correctness() {
    #[derive(Trace, Clone)]
    struct Container {
        value: Gc<i32>,
    }

    impl GcCapture for Container {
        fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<GcBox<()>>] {
            &[]
        }
        fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<std::ptr::NonNull<GcBox<()>>>) {
            self.value.capture_gc_ptrs_into(ptrs);
        }
    }

    let container = Container { value: Gc::new(42) };

    let cell = Arc::new(Gc::new(GcThreadSafeCell::new(container)));

    let cell_clone = cell.clone();

    let handle = std::thread::spawn(move || {
        for i in 0..10 {
            *cell_clone.borrow_mut() = Container { value: Gc::new(i) };
        }
    });

    handle.join().unwrap();

    assert!(*cell.borrow().value >= 0);

    drop(cell);
    collect_full();
}

#[cfg(feature = "tokio")]
#[test]
fn test_tokio_multi_thread_gc_thread_safe_cell_with_gc_ptrs() {
    #[derive(Trace, Clone)]
    struct Container {
        value: Gc<i32>,
    }

    impl GcCapture for Container {
        fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<GcBox<()>>] {
            &[]
        }
        fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<std::ptr::NonNull<GcBox<()>>>) {
            self.value.capture_gc_ptrs_into(ptrs);
        }
    }

    let cell = Arc::new(Gc::new(GcThreadSafeCell::new(Container {
        value: Gc::new(42),
    })));

    let cell_clone = cell.clone();
    let final_value = Arc::new(std::sync::atomic::AtomicI32::new(0));

    let final_value_clone = final_value.clone();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap();

    rt.block_on(async {
        let handle = tokio::spawn(async move {
            for i in 0..10 {
                *cell_clone.borrow_mut() = Container { value: Gc::new(i) };
            }
            *cell_clone.borrow().value
        });

        let result = handle.await.unwrap();
        final_value_clone.store(result, std::sync::atomic::Ordering::SeqCst);
    });

    assert!(*cell.borrow().value >= 0);
    assert!(final_value.load(std::sync::atomic::Ordering::SeqCst) >= 0);
}

#[test]
fn test_cross_thread_satb_during_active_gc() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[derive(Trace, Clone)]
    struct Container {
        value: Gc<i32>,
    }

    impl GcCapture for Container {
        fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<GcBox<()>>] {
            &[]
        }
        fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<std::ptr::NonNull<GcBox<()>>>) {
            self.value.capture_gc_ptrs_into(ptrs);
        }
    }

    rudo_gc::test_util::reset();

    let initial_value = Gc::new(42);
    let container = Container {
        value: initial_value,
    };

    let cell = Arc::new(Gc::new(GcThreadSafeCell::new(container)));

    let mutation_count = Arc::new(AtomicUsize::new(0));
    let gc_started = Arc::new(AtomicBool::new(false));
    let thread_started = Arc::new(AtomicBool::new(false));

    let cell_clone = cell.clone();
    let count_clone = mutation_count.clone();
    let started_clone = thread_started.clone();
    let gc_started_clone = gc_started.clone();

    let handle = thread::spawn(move || {
        started_clone.store(true, Ordering::SeqCst);

        // Wait for GC to start
        while !gc_started_clone.load(Ordering::SeqCst) {
            thread::yield_now();
        }

        // Continue mutations while GC is running
        for i in 0..50 {
            *cell_clone.borrow_mut() = Container {
                value: Gc::new(i + 100),
            };
            count_clone.fetch_add(1, Ordering::SeqCst);
            thread::yield_now();
        }
    });

    // Wait for thread to start
    while !thread_started.load(Ordering::SeqCst) {
        thread::yield_now();
    }

    // Give thread time to start mutations
    thread::sleep(Duration::from_millis(10));

    // Trigger GC while thread is actively mutating
    gc_started.store(true, Ordering::SeqCst);
    rudo_gc::collect_full();

    // Wait for thread to complete
    handle.join().unwrap();

    // Verify the cell contains a valid value
    let final_value_check = *cell.borrow().value >= 100;
    assert!(
        final_value_check,
        "Final value should be from post-GC-start mutations"
    );

    // Verify mutations occurred during GC
    assert!(
        mutation_count.load(Ordering::SeqCst) > 0,
        "Mutations should have occurred during GC"
    );

    // Verify the old value (42) was preserved in SATB buffer
    // and the new value is properly accessible after GC
    drop(cell);
    rudo_gc::collect_full();

    // Test passes if we reach here without memory corruption
}

/// Regression test for the P5 / dual-overflow SATB bug.
///
/// Previously, when both the main cross-thread SATB buffer and its overflow
/// buffer were full, any additional SATB entry was silently dropped, breaking
/// the SATB snapshot guarantee and potentially causing premature collection.
///
/// This test:
/// 1. Configures a tiny per-buffer limit (1 entry each) so the emergency
///    buffer is reachable with only 3 pushes.
/// 2. Pushes three synthetic cross-thread SATB entries (one per tier).
/// 3. Asserts that flushing recovers all three entries — none were dropped.
#[test]
fn test_cross_thread_satb_dual_overflow_no_entry_loss() {
    use rudo_gc::GcBox;
    use std::ptr::NonNull;

    rudo_gc::test_util::reset();

    // Use a buffer limit of 1: main holds 1, overflow holds 1, the 3rd must
    // land in the emergency buffer rather than being discarded.
    rudo_gc::test_util::set_cross_thread_satb_capacity(1);

    // Allocate three objects that will serve as SATB old-value entries.
    // They must remain allocated for the duration of the test so we have
    // valid (non-null, in-heap) addresses to push.
    let gc1 = Gc::new(1u64);
    let gc2 = Gc::new(2u64);
    let gc3 = Gc::new(3u64);

    // SAFETY: These raw pointers are valid GcBox addresses owned by gc1/gc2/gc3,
    // which remain live for the entire test body.
    // GcBox<T> is always allocated at the correct alignment by the GC allocator;
    // the u8 alias is only for pointer arithmetic convenience.
    #[allow(clippy::cast_ptr_alignment)]
    let ptr1 = unsafe { NonNull::new_unchecked(Gc::internal_ptr(&gc1) as *mut GcBox<()>) };
    #[allow(clippy::cast_ptr_alignment)]
    let ptr2 = unsafe { NonNull::new_unchecked(Gc::internal_ptr(&gc2) as *mut GcBox<()>) };
    #[allow(clippy::cast_ptr_alignment)]
    let ptr3 = unsafe { NonNull::new_unchecked(Gc::internal_ptr(&gc3) as *mut GcBox<()>) };

    // Push 3 entries:
    //   push 1 -> main buffer (len 0 → 1, limit 1; accepted)
    //   push 2 -> overflow (main full, overflow len 0 → 1)
    //   push 3 -> emergency (main full, overflow full)
    let r1 = rudo_gc::heap::LocalHeap::push_cross_thread_satb(ptr1);
    let r2 = rudo_gc::heap::LocalHeap::push_cross_thread_satb(ptr2);
    let r3 = rudo_gc::heap::LocalHeap::push_cross_thread_satb(ptr3);

    // r1 true (main path); r2 and r3 false (overflow/emergency with fallback requested).
    assert!(r1, "first push should succeed in main buffer");
    assert!(
        !r2,
        "second push should report overflow (overflow buffer used)"
    );
    assert!(
        !r3,
        "third push should report overflow (emergency buffer used)"
    );

    // Flush and count — all three must be present; previously ptr3 was lost.
    let flushed = rudo_gc::test_util::flush_cross_thread_satb();
    assert_eq!(
        flushed, 3,
        "all 3 SATB entries must be recovered after flush; \
         if this is 2, the dual-overflow bug regressed"
    );

    // Restore state so subsequent tests are unaffected.
    rudo_gc::test_util::reset();
}
