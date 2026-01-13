//! Regression tests for sweep phase issues.

use rudo_gc::{collect_full, Gc, Trace, Visitor};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[test]
fn test_drop_allocates() {
    DROP_COUNT.with(|c| c.set(0));

    // Allocate multiple objects to increase chance of triggering Vec reallocation
    let mut objects = Vec::new();
    for _ in 0..100 {
        objects.push(Gc::new(AllocatingDropper));
    }

    drop(objects);
    collect_full();

    // Verify all droppers were dropped successfully
    assert_eq!(DROP_COUNT.with(Cell::get), 100);
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
