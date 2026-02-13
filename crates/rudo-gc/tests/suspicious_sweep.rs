#![cfg(feature = "debug-suspicious-sweep")]

use rudo_gc::{Gc, GcCell, Trace};

#[derive(Trace)]
struct TestItem {
    value: i32,
}

#[test]
fn test_gc_vec_gc_works_correctly() {
    let items: Gc<GcCell<Vec<Gc<TestItem>>>> = Gc::new(GcCell::new(Vec::new()));

    for i in 0..10 {
        items.borrow_mut().push(Gc::new(TestItem { value: i }));
    }

    assert_eq!(items.borrow().len(), 10);

    rudo_gc::collect_full();

    assert_eq!(items.borrow().len(), 10);
    for (i, item) in items.borrow().iter().enumerate() {
        assert_eq!(item.value, i32::try_from(i).unwrap());
    }
}

#[test]
fn test_detection_can_be_toggled() {
    assert!(rudo_gc::is_suspicious_sweep_detection_enabled());

    rudo_gc::set_suspicious_sweep_detection(false);
    assert!(!rudo_gc::is_suspicious_sweep_detection_enabled());

    rudo_gc::set_suspicious_sweep_detection(true);
    assert!(rudo_gc::is_suspicious_sweep_detection_enabled());
}

#[test]
fn test_gc_weak_ref_works() {
    use rudo_gc::Weak;

    let strong: Gc<GcCell<Vec<Gc<TestItem>>>> = Gc::new(GcCell::new(Vec::new()));
    let weak: Weak<GcCell<Vec<Gc<TestItem>>>> = rudo_gc::Gc::downgrade(&strong);

    strong.borrow_mut().push(Gc::new(TestItem { value: 42 }));

    rudo_gc::collect_full();

    assert!(weak.upgrade().is_some());
    assert_eq!(weak.upgrade().unwrap().borrow().len(), 1);
}

#[test]
fn test_multiple_gc_cycles_preserve_data() {
    let items: Gc<GcCell<Vec<Gc<TestItem>>>> = Gc::new(GcCell::new(Vec::new()));

    for round in 0..3 {
        for i in 0..5 {
            items.borrow_mut().push(Gc::new(TestItem {
                value: round * 100 + i,
            }));
        }
        rudo_gc::collect_full();
    }

    assert_eq!(items.borrow().len(), 15);
}

#[test]
fn test_promoted_objects_not_detected() {
    let item: Gc<GcCell<TestItem>> = Gc::new(GcCell::new(TestItem { value: 42 }));

    rudo_gc::collect_full();
    rudo_gc::collect_full();

    assert_eq!(item.borrow().value, 42);
}

#[test]
fn test_simple_gc_allocation() {
    let gc = Gc::new(TestItem { value: 123 });
    assert_eq!(gc.value, 123);

    rudo_gc::collect_full();

    assert_eq!(gc.value, 123);
}
