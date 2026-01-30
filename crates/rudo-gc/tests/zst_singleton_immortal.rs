//! Tests for ZST singleton immortality guarantee.
//!
//! These tests verify that the ZST singleton (used for `Gc<()>` and other ZSTs)
//! survives GC cycles and never gets reclaimed, preventing ABA safety issues.

use rudo_gc::{collect_full, Gc, Trace, Weak};

#[test]
fn test_zst_singleton_survives_collect() {
    let unit = Gc::new(());
    drop(unit);
    collect_full();

    let unit2 = Gc::new(());
    assert!(Gc::ptr_eq(&unit2, &unit2));
    drop(unit2);
}

#[test]
fn test_zst_singleton_no_uaf_after_multiple_cycles() {
    for _ in 0..10 {
        let unit = Gc::new(());
        drop(unit);
        collect_full();
    }

    let unit = Gc::new(());
    drop(unit);
    collect_full();
}

#[test]
fn test_zst_singleton_same_pointer() {
    let units: Vec<Gc<()>> = (0..10).map(|_| Gc::new(())).collect();

    for unit in &units {
        assert!(Gc::ptr_eq(unit, &units[0]));
    }
}

#[test]
fn test_zst_singleton_ref_count_maintained() {
    let unit = Gc::new(());
    let initial_rc = Gc::ref_count(&unit).get();

    let clone1 = Gc::clone(&unit);
    assert_eq!(Gc::ref_count(&unit).get(), initial_rc + 1);

    let clone2 = Gc::clone(&unit);
    assert_eq!(Gc::ref_count(&unit).get(), initial_rc + 2);

    drop(clone1);
    assert_eq!(Gc::ref_count(&unit).get(), initial_rc + 1);

    drop(clone2);
    assert_eq!(Gc::ref_count(&unit).get(), initial_rc);
}

#[test]
fn test_zst_weak_ref_behavior() {
    let weak: Weak<()> = Gc::downgrade(&Gc::new(()));

    assert!(weak.upgrade().is_some());

    drop(Gc::downgrade(&Gc::new(())).upgrade().unwrap());
    collect_full();

    assert!(weak.upgrade().is_some());
}

#[test]
fn test_zst_weak_ref_to_singleton() {
    let weak: Weak<()> = Gc::downgrade(&Gc::new(()));

    let gc1 = weak.upgrade().unwrap();
    let gc2 = weak.upgrade().unwrap();

    assert!(Gc::ptr_eq(&gc1, &gc2));
}

#[test]
fn test_zst_singleton_concurrent_init() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    let inits = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(10));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let barrier = barrier.clone();
            let inits = inits.clone();
            thread::spawn(move || {
                barrier.wait();
                let unit = Gc::new(());
                drop(unit);
                inits.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(inits.load(Ordering::SeqCst), 10);
}

#[test]
fn test_zst_singleton_after_thread_termination() {
    use std::thread;

    let handle = thread::spawn(|| {
        for _ in 0..100 {
            let _ = Gc::new(());
        }
    });
    handle.join().unwrap();

    let unit = Gc::new(());
    drop(unit);
}

#[test]
fn test_custom_zst_singleton() {
    #[derive(Debug, Clone, Copy, PartialEq, Trace)]
    struct Empty;

    let empty1 = Gc::new(Empty);
    let empty2 = Gc::new(Empty);

    assert!(Gc::ptr_eq(&empty1, &empty2));

    let empty1_clone = empty1.clone();
    drop(empty1);
    collect_full();

    let empty3 = Gc::new(Empty);
    assert!(Gc::ptr_eq(&empty1_clone, &empty3));
}
