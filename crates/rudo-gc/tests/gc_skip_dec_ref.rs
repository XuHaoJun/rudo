use rudo_gc::{collect, Gc, Trace};
use std::sync::atomic::AtomicUsize;

static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Trace)]
struct TrackedDrop {
    value: usize,
}

impl Drop for TrackedDrop {
    fn drop(&mut self) {
        DROP_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn test_gc_skipped_dec_ref_causes_leak() {
    DROP_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);

    {
        let _gc = Gc::new(TrackedDrop { value: 42 });

        collect();

        assert_eq!(
            DROP_COUNT.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Object should NOT be dropped yet (Gc still in scope)"
        );
    }

    collect();

    assert_eq!(
        DROP_COUNT.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Object should be dropped when it goes out of scope (dec_ref should be called)"
    );
}

#[test]
fn test_weak_ref_sees_collected_object() {
    DROP_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);

    let strong = Gc::new(TrackedDrop { value: 200 });
    let weak = Gc::downgrade(&strong);

    assert!(weak.upgrade().is_some());

    drop(strong);

    collect();

    assert_eq!(
        DROP_COUNT.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Object should be dropped and weak ref should see None"
    );

    assert!(
        weak.upgrade().is_none(),
        "Weak ref should not upgrade after collection"
    );
}

#[test]
fn test_simple_clone_and_drop() {
    DROP_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);

    let gc = Gc::new(TrackedDrop { value: 100 });
    let gc_ref = Gc::clone(&gc);

    assert_eq!(
        Gc::ref_count(&gc).get(),
        2,
        "Clone should increase ref count to 2"
    );

    drop(gc_ref);

    assert_eq!(
        Gc::ref_count(&gc).get(),
        1,
        "Dropping clone should decrease ref count to 1"
    );

    drop(gc);

    assert_eq!(
        DROP_COUNT.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Object should be dropped when last ref is gone"
    );
}
