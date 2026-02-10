//! Minimal reproduction of Vec<Gc<T>> corruption bug
//!
//! This test demonstrates memory corruption in Vec<Gc<T>> when GC runs during
//! component creation. The bug manifests as data from later rounds overwriting
//! data from earlier rounds.
//!
//! Run: cargo test --test vec_gc_corruption_minimal -- --test-threads=1

use rudo_gc::{collect, Gc, GcCell, Trace};
use rudo_gc_derive::GcCell;
use std::sync::atomic::AtomicBool;

#[derive(Trace)]
pub struct Component {
    pub id: u64,
    pub parent: GcCell<Option<Gc<Self>>>,
    pub is_dirty: AtomicBool,
}

impl Component {
    pub fn new(id: u64) -> Gc<Self> {
        Gc::new(Self {
            id,
            parent: GcCell::new(None),
            is_dirty: AtomicBool::new(false),
        })
    }
}

#[derive(Trace, GcCell)]
pub struct Container {
    pub items: GcCell<Vec<Gc<Component>>>,
}

impl Container {
    pub fn new() -> Gc<Self> {
        Gc::new(Self {
            items: GcCell::new(Vec::new()),
        })
    }
}

/// Test that passes - no GC during component creation
#[test]
#[cfg_attr(miri, ignore)]
fn test_vec_no_auto_gc() {
    rudo_gc::set_collect_condition(|_| false);

    let container = Container::new();

    for round in 0..15 {
        for i in 0..20 {
            let id = (round * 1000 + i) as u64;
            let component = Component::new(id);
            *component.parent.borrow_mut() = None;
            container.items.borrow_mut().push(Gc::clone(&component));
        }

        for (idx, comp) in container.items.borrow().iter().enumerate() {
            let expected_round = idx / 20;
            let expected_local = idx % 20;
            let expected_id = (expected_round * 1000 + expected_local) as u64;
            assert_eq!(
                comp.id, expected_id,
                "Round {}: idx {} corrupted! Expected {}, got {}",
                round, idx, expected_id, comp.id
            );
        }
    }

    for (idx, comp) in container.items.borrow().iter().enumerate() {
        let expected_round = idx / 20;
        let expected_local = idx % 20;
        let expected_id = (expected_round * 1000 + expected_local) as u64;
        assert_eq!(comp.id, expected_id, "Final idx {} corrupted", idx);
    }

    collect();
    drop(container);
    collect();

    rudo_gc::set_collect_condition(rudo_gc::default_collect_condition);
}

/// Test that fails - GC during component creation causes corruption
#[test]
#[cfg_attr(miri, ignore)]
fn test_vec_gc_corruption_with_barriers() {
    rudo_gc::set_collect_condition(|_| false);

    let container = Container::new();

    for round in 0..15 {
        for i in 0..20 {
            let id = (round * 1000 + i) as u64;
            let component = Component::new(id);
            *component.parent.borrow_mut() = None;
            container.items.borrow_mut().push(Gc::clone(&component));
        }

        for (idx, comp) in container.items.borrow().iter().enumerate() {
            let expected_round = idx / 20;
            let expected_local = idx % 20;
            let expected_id = (expected_round * 1000 + expected_local) as u64;
            assert_eq!(
                comp.id, expected_id,
                "Round {}: idx {} corrupted! Expected {}, got {}",
                round, idx, expected_id, comp.id
            );
        }

        if round % 5 == 4 {
            collect();
        }
    }

    for (idx, comp) in container.items.borrow().iter().enumerate() {
        let expected_round = idx / 20;
        let expected_local = idx % 20;
        let expected_id = (expected_round * 1000 + expected_local) as u64;
        assert_eq!(comp.id, expected_id, "Final idx {} corrupted", idx);
    }

    collect();
    drop(container);
    collect();

    rudo_gc::set_collect_condition(rudo_gc::default_collect_condition);
}
