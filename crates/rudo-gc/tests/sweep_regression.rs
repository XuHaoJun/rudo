//! Regression tests for sweep phase issues.

use rudo_gc::cell::GcCell;
use rudo_gc::{collect_full, set_suspicious_sweep_detection, Gc, Trace, Visitor};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

static ORPHAN_DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static DROP_COUNT: Cell<usize> = const { Cell::new(0) };
}

struct AllocatingDropper;

unsafe impl Trace for AllocatingDropper {
    fn trace(&self, _: &mut impl Visitor) {}
}

impl Drop for AllocatingDropper {
    fn drop(&mut self) {
        // Allocate new objects during drop
        let _ = Gc::new(12345i32);
        DROP_COUNT.with(|c| c.set(c.get() + 1));
    }
}

/// Test that `drop_fn` can safely trigger allocation.
///
/// This verifies the snapshot pattern works - `drop_fn` allocates new objects,
/// but the original page iteration continues safely using the snapshot.
/// See `docs/reentrant-alloc-rules.md` for context.
#[test]
fn test_drop_allocates() {
    set_suspicious_sweep_detection(false);
    DROP_COUNT.with(|c| c.set(0));

    // Allocate multiple objects to increase chance of triggering Vec reallocation
    let mut objects = Vec::new();
    for _ in 0..100 {
        objects.push(Gc::new(AllocatingDropper));
    }

    drop(objects);

    // CRITICAL: Clear callee-saved registers to prevent stale pointer values
    // from being treated as roots by the conservative GC.
    unsafe { rudo_gc::test_util::clear_registers() };

    collect_full();

    // Verify all droppers were dropped successfully
    assert_eq!(DROP_COUNT.with(Cell::get), 100);
    set_suspicious_sweep_detection(true);
}

struct DropCounter;

unsafe impl Trace for DropCounter {
    fn trace(&self, _: &mut impl Visitor) {}
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        ORPHAN_DROP_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn test_orphan_page_drop() {
    let handle = thread::spawn(|| {
        // Allocate objects in child thread
        // These objects should be dropped when thread terminates
        // and orphan pages are swept
        for _ in 0..50 {
            let _ = Gc::new(DropCounter);
        }
        // Thread terminates, pages become orphan
    });

    handle.join().unwrap();

    // Trigger GC - should drop all 50 DropCounter objects
    collect_full();

    // All DropCounter objects should be dropped
    assert_eq!(ORPHAN_DROP_COUNT.load(Ordering::SeqCst), 50);
}

struct CrossNode {
    other: GcCell<Option<Gc<Self>>>,
    _data: [u8; 100], // Make it large-ish to ensure different slots
}

unsafe impl Trace for CrossNode {
    fn trace(&self, visitor: &mut impl Visitor) {
        self.other.trace(visitor);
    }
}

#[test]
fn test_orphan_cross_page_reference() {
    let barrier = Arc::new(Barrier::new(2));

    // We'll use a raw pointer to pass the Gc between threads because Gc is !Send.
    // This is "safe" in this test context because we're just setting up a reference
    // graph and then letting both threads die, turning them into orphans.
    let node_a_raw = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let b1 = barrier.clone();
    let n1 = node_a_raw.clone();
    let h1 = thread::spawn(move || {
        let node_a = Gc::new(CrossNode {
            other: GcCell::new(None),
            _data: [0; 100],
        });
        n1.store(Gc::internal_ptr(&node_a) as usize, Ordering::SeqCst);

        b1.wait(); // Wait for thread 2 to see node_a
        b1.wait(); // Wait for thread 2 to finish linking
    });

    let h2 = thread::spawn(move || {
        barrier.wait(); // Wait for thread 1 to create node_a

        let node_a_ptr = node_a_raw.load(Ordering::SeqCst) as *const u8;
        // SAFETY: We're reconstructng a Gc from a raw pointer in a different thread.
        // NEVER DO THIS IN REAL CODE. Here it's to force a cross-thread/cross-page orphan graph.
        let node_a = unsafe { Gc::<CrossNode>::from_raw(node_a_ptr) };

        let node_b = Gc::new(CrossNode {
            other: GcCell::new(Some(node_a)),
            _data: [0; 100],
        });

        drop(node_b);
        barrier.wait(); // Signal thread 1 we are done
    });

    h1.join().unwrap();
    h2.join().unwrap();

    // Now node_a and node_b are orphans. node_b points to node_a.
    // Triggering GC will call sweep_orphan_pages.
    collect_full();
}
