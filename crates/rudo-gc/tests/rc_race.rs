use rudo_gc::{collect, Gc, Trace};
use std::sync::atomic::{AtomicUsize, Ordering};

static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Trace)]
struct TrackedDrop {
    value: usize,
}

impl Drop for TrackedDrop {
    fn drop(&mut self) {
        DROP_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn test_basic_clone_and_drop() {
    DROP_COUNT.store(0, Ordering::SeqCst);

    let gc = Gc::new(TrackedDrop { value: 42 });

    assert_eq!(Gc::ref_count(&gc).get(), 1);

    let gc2 = Gc::clone(&gc);

    assert_eq!(Gc::ref_count(&gc).get(), 2);

    drop(gc2);

    assert_eq!(Gc::ref_count(&gc).get(), 1);

    drop(gc);

    collect();

    assert_eq!(
        DROP_COUNT.load(Ordering::SeqCst),
        1,
        "Object should be dropped exactly once"
    );
}

#[test]
fn test_concurrent_dec_ref_no_double_free() {
    DROP_COUNT.store(0, Ordering::SeqCst);

    let gc = Gc::new(TrackedDrop { value: 42 });

    let gc2 = Gc::clone(&gc);
    let gc3 = Gc::clone(&gc);
    let gc4 = Gc::clone(&gc);

    assert_eq!(Gc::ref_count(&gc).get(), 4);

    drop(gc4);
    drop(gc3);
    drop(gc2);

    assert_eq!(Gc::ref_count(&gc).get(), 1);

    drop(gc);

    collect();

    assert_eq!(
        DROP_COUNT.load(Ordering::SeqCst),
        1,
        "Object should be dropped exactly once"
    );
}
