//! Tests for external reference tracking with Gc<Rc<T>>.
//!
//! These tests verify GC correctness by storing `Gc<Rc<T>>` where `Rc<T>`
//! provides external reference counting. This pattern allows verifying that
//! objects are correctly collected when roots are dropped.

#![cfg(feature = "test-util")]

use std::cell::Cell;
use std::rc::Rc;

use rudo_gc::collect_full;
use rudo_gc::{Gc, Trace};

#[derive(Debug, Clone)]
struct DropTracker {
    marker: Rc<Cell<bool>>,
}

impl DropTracker {
    const fn new(marker: Rc<Cell<bool>>) -> Self {
        Self { marker }
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.marker.set(true);
    }
}

unsafe impl Trace for DropTracker {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[derive(Debug, Clone)]
struct RefCounter {
    value: i32,
}

unsafe impl Trace for RefCounter {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[test]
fn test_simple_allocation() {
    let gc = Gc::new(RefCounter { value: 42 });
    assert_eq!(gc.value, 42);
    drop(gc);
    collect_full();
}

#[test]
fn test_external_rc_strong_count() {
    let rc = Rc::new(Cell::new(0));
    let gc = Gc::new(rc.clone());

    let initial_count = Rc::strong_count(&rc);
    assert_eq!(
        initial_count, 2,
        "Gc<Rc<T>> starts with 2 refs (Gc + external)"
    );

    drop(gc);
    collect_full();

    let after_collection = Rc::strong_count(&rc);
    assert_eq!(
        after_collection, 1,
        "After dropping Gc, should have 1 ref (external only)"
    );
}

#[test]
fn test_multiple_external_refs() {
    let rc = Rc::new(Cell::new(0));
    let gc = Gc::new(rc.clone());
    let rc1 = Rc::clone(&rc);
    let rc2 = Rc::clone(&rc);
    let rc3 = Rc::clone(&rc);

    assert_eq!(Rc::strong_count(&rc), 5, "Gc + original + 3 clones");

    drop(rc2);
    assert_eq!(Rc::strong_count(&rc), 4);

    drop(rc1);
    assert_eq!(Rc::strong_count(&rc), 3);

    drop(rc3);
    assert_eq!(Rc::strong_count(&rc), 2);

    drop(gc);
    collect_full();

    let after_collection = Rc::strong_count(&rc);
    assert_eq!(after_collection, 1, "After dropping Gc, should have 1 ref");
}

#[test]
fn test_drop_behavior_verified() {
    let marker = Rc::new(Cell::new(false));
    let gc = Gc::new(DropTracker::new(marker.clone()));
    assert!(!marker.get());

    drop(gc);
    collect_full();

    assert!(marker.get());
}

#[test]
fn test_drop_behavior_with_rc() {
    let marker = Rc::new(Cell::new(false));
    let gc = Gc::new(DropTracker::new(marker.clone()));
    assert!(!marker.get());

    let cloned_rc = Rc::clone(&marker);
    assert_eq!(Rc::strong_count(&marker), 3, "Gc + original + 1 clone");

    drop(cloned_rc);
    assert_eq!(Rc::strong_count(&marker), 2, "Gc + original");

    drop(gc);
    collect_full();

    assert!(marker.get());
}

#[test]
fn test_repeated_allocation_deallocation() {
    const COUNT: usize = 1000;

    #[allow(unused_variables)]
    for _ in 0..COUNT {
        let rc = Rc::new(Cell::new(0));
        let _gc = Gc::new(rc.clone());
        assert_eq!(Rc::strong_count(&rc), 2);
    }

    collect_full();
}

#[test]
fn test_rc_wrapper_preserves_content() {
    let rc = Rc::new(Cell::new(0));
    let gc = Gc::new(rc.clone());
    assert_eq!(Rc::strong_count(&rc), 2);

    drop(gc);
    collect_full();

    assert_eq!(Rc::strong_count(&rc), 1);
}

#[test]
fn test_nested_rc_tracking() {
    struct Inner {
        value: Rc<Cell<i32>>,
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            self.value.set(self.value.get() - 1);
        }
    }

    unsafe impl Trace for Inner {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    let value = Rc::new(Cell::new(5));
    let gc = Gc::new(Inner {
        value: value.clone(),
    });

    assert_eq!(value.get(), 5);

    drop(gc);
    collect_full();

    assert_eq!(value.get(), 4);
}

#[test]
fn test_shared_rc_across_multiple_gc() {
    let shared_rc = Rc::new(Cell::new(0));

    {
        let gc1 = Gc::new(shared_rc.clone());
        let gc2 = Gc::new(shared_rc.clone());
        let gc3 = Gc::new(shared_rc.clone());

        assert_eq!(Rc::strong_count(&shared_rc), 4, "3 Gc + 1 original");

        drop(gc1);
        assert_eq!(Rc::strong_count(&shared_rc), 3);

        drop(gc2);
        assert_eq!(Rc::strong_count(&shared_rc), 2);

        drop(gc3);
        assert_eq!(Rc::strong_count(&shared_rc), 1);
    }

    collect_full();

    assert_eq!(Rc::strong_count(&shared_rc), 1, "Original still exists");
}

#[test]
fn test_massive_external_ref_test() {
    const COUNT: usize = 10_000;

    let mut markers: Vec<Rc<Cell<bool>>> = Vec::with_capacity(COUNT);

    for i in 0..COUNT {
        let marker = Rc::new(Cell::new(false));
        let gc = Gc::new(DropTracker::new(marker.clone()));
        markers.push(marker);

        if i % 1000 == 0 {
            drop(gc);
        } else {
            std::mem::forget(gc);
        }
    }

    assert_eq!(markers.len(), COUNT);

    drop(markers);
    collect_full();
}

#[test]
fn test_weak_references_with_external_tracking() {
    let rc = Rc::new(Cell::new(0));
    let gc = Gc::new(rc.clone());
    let weak = Gc::downgrade(&gc);

    assert!(weak.is_alive());

    let external_rc = Rc::clone(&rc);
    assert_eq!(Rc::strong_count(&rc), 3);

    drop(external_rc);
    assert_eq!(Rc::strong_count(&rc), 2);

    drop(gc);
    collect_full();

    assert!(!weak.is_alive());
}

#[test]
fn test_gc_object_stores_external_rc() {
    struct Container {
        external: Rc<Cell<u32>>,
        #[allow(dead_code)]
        gc_ref: Gc<RefCounter>,
    }

    impl Drop for Container {
        fn drop(&mut self) {
            self.external.set(self.external.get() + 1);
        }
    }

    unsafe impl Trace for Container {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    let gc = Gc::new(RefCounter { value: 5 });
    let external = Rc::new(Cell::new(0));

    {
        let container = Gc::new(Container {
            external: external.clone(),
            gc_ref: gc.clone(),
        });

        assert_eq!(external.get(), 0);
        assert_eq!(gc.value, 5);

        drop(container);
        assert_eq!(external.get(), 1);
    }

    collect_full();

    assert_eq!(external.get(), 1);
}

#[test]
fn test_collection_stress_with_mixed_types() {
    const ITERATIONS: usize = 100;

    #[allow(unused_variables, clippy::used_underscore_binding)]
    for _i in 0..ITERATIONS {
        let rc1 = Rc::new(Cell::new(0));
        let gc1 = Gc::new(rc1.clone());
        let _gc2 = Gc::new(DropTracker::new(Rc::new(Cell::new(false))));
        let gc3 = Gc::new(rc1.clone());

        assert_eq!(Rc::strong_count(&rc1), 3);

        if _i % 2 == 0 {
            drop(gc1);
        } else {
            drop(gc3);
        }
    }

    collect_full();
}

#[test]
fn test_verify_all_objects_collected() {
    const COUNT: usize = 500;

    let mut external_refs: Vec<Rc<Cell<u32>>> = Vec::new();

    #[allow(unused_variables)]
    for _ in 0..COUNT {
        let rc = Rc::new(Cell::new(0));
        let _gc = Gc::new(rc.clone());
        external_refs.push(rc);
    }

    for (i, rc) in external_refs.iter().enumerate() {
        assert_eq!(rc.get(), 0, "Object {i} should not have been dropped yet");
    }

    drop(external_refs);
    collect_full();
}

#[test]
fn test_partial_drop_then_collect() {
    const COUNT: usize = 100;

    let mut gc_refs: Vec<Gc<RefCounter>> = Vec::new();

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::needless_range_loop
    )]
    for i in 0..COUNT {
        let gc = Gc::new(RefCounter { value: i as i32 });
        gc_refs.push(gc);
    }

    #[allow(clippy::needless_range_loop)]
    for i in 0..COUNT / 2 {
        gc_refs[i] = Gc::new(RefCounter { value: -1 });
    }

    gc_refs.truncate(COUNT / 2);

    #[allow(clippy::uninlined_format_args)]
    for (i, gc) in gc_refs.iter().enumerate() {
        assert_eq!(gc.value, -1, "Index {i}");
    }

    drop(gc_refs);
    collect_full();
}

#[test]
fn test_nested_structure_with_external_tracking() {
    use std::cell::RefCell;

    struct Node {
        children: RefCell<Vec<Gc<RefCounter>>>,
    }

    unsafe impl Trace for Node {
        fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
    }

    let root = Gc::new(Node {
        children: RefCell::new(Vec::new()),
    });

    for i in 1..100 {
        let child = Gc::new(RefCounter { value: i });
        root.children.borrow_mut().push(child);
    }

    assert_eq!(root.children.borrow().len(), 99);

    drop(root);
    collect_full();
}

#[test]
fn test_arena_lifecycle_complete() {
    const SIZE: usize = 50;

    let mut markers: Vec<Rc<Cell<bool>>> = Vec::new();

    for i in 0..SIZE {
        let marker = Rc::new(Cell::new(false));
        let gc = Gc::new(DropTracker::new(marker.clone()));
        assert!(!marker.get());

        if i % 10 == 0 {
            markers.push(marker);
        } else {
            std::mem::forget(gc);
        }
    }

    assert_eq!(markers.len(), SIZE / 10);

    drop(markers);
    collect_full();
}

#[test]
fn test_external_ref_with_nested_gc() {
    let marker = Rc::new(Cell::new(false));
    let gc = Gc::new(DropTracker::new(marker.clone()));
    assert!(!marker.get());

    drop(gc);
    collect_full();

    assert!(marker.get());
}
