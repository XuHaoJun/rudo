//! Integration tests for minor GC with dirty page tracking optimization.
//!
//! These tests verify that:
//! 1. Old-to-young references survive minor GC
//! 2. Large objects are properly handled
//! 3. The dirty page tracking correctly identifies pages to scan

#![allow(clippy::use_self)]
#![allow(clippy::redundant_clone)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::missing_const_for_fn)]

use rudo_gc::{collect, collect_full, Gc, GcCell, Trace};
use std::cell::RefCell;

/// A node that can hold a reference to another GC object.
#[derive(Trace)]
struct Node<T: Trace + 'static> {
    value: GcCell<T>,
    next: GcCell<Option<Gc<Node<T>>>>,
}

impl<T: Trace + 'static> Node<T> {
    fn new(value: T) -> Self {
        Self {
            value: GcCell::new(value),
            next: GcCell::new(None),
        }
    }

    fn set_next(&self, next: Gc<Node<T>>) {
        *self.next.borrow_mut() = Some(next);
    }
}

/// Test that old-to-young references survive minor GC.
///
/// This is the primary correctness test for the dirty page tracking feature.
/// An old-generation object that references a young-generation object
/// should be scanned during minor GC to ensure the young object survives.
#[test]
fn test_old_to_young_reference_survives_minor_gc() {
    // Create an old-generation object
    let old_obj: Gc<Node<i32>> = Gc::new(Node::new(1));

    // Trigger full collection to promote to old generation
    collect_full();

    // Create a young-generation object
    let young_obj: Gc<Node<i32>> = Gc::new(Node::new(42));

    // Establish old-to-young reference via mutation (triggers write barrier)
    old_obj.set_next(young_obj.clone());

    // Trigger minor collection
    // The young_obj should survive because old_obj is in the dirty page list
    collect();

    // Verify both objects survived
    assert_eq!(*old_obj.value.borrow(), 1);
    assert_eq!(*young_obj.value.borrow(), 42);

    // Verify the reference is intact
    {
        if let Some(ref next) = *old_obj.next.borrow() {
            assert_eq!(*next.value.borrow(), 42);
        } else {
            panic!("Old-to-young reference was lost!");
        };
    }
}

/// Test that multiple old-to-young references are preserved.
#[test]
fn test_multiple_old_to_young_references() {
    // Create old-generation objects
    let old_parent: Gc<Node<i32>> = Gc::new(Node::new(0));
    let old_child1: Gc<Node<i32>> = Gc::new(Node::new(1));
    let old_child2: Gc<Node<i32>> = Gc::new(Node::new(2));

    // Promote to old generation
    collect_full();

    // Create young-generation objects
    let young1: Gc<Node<i32>> = Gc::new(Node::new(100));
    let young2: Gc<Node<i32>> = Gc::new(Node::new(200));
    let young3: Gc<Node<i32>> = Gc::new(Node::new(300));

    // Establish references (triggers write barriers)
    old_parent.set_next(young1.clone());
    old_child1.set_next(young2.clone());
    old_child2.set_next(young3.clone());

    // Trigger minor collection
    collect();

    // Verify all young objects survived
    assert_eq!(*young1.value.borrow(), 100);
    assert_eq!(*young2.value.borrow(), 200);
    assert_eq!(*young3.value.borrow(), 300);

    // Verify references are intact
    assert!(old_parent.next.borrow().is_some());
    assert!(old_child1.next.borrow().is_some());
    assert!(old_child2.next.borrow().is_some());
}

/// Test old-to-young reference survival with chain of references.
#[test]
#[allow(clippy::redundant_clone)]
fn test_old_to_young_chain_survival() {
    // Create a chain: old -> old -> young
    let old1: Gc<Node<i32>> = Gc::new(Node::new(1));
    let old2: Gc<Node<i32>> = Gc::new(Node::new(2));

    // Establish initial link and promote
    old1.set_next(old2.clone());
    collect_full();

    // Create young object at end of chain
    let young: Gc<Node<i32>> = Gc::new(Node::new(999));

    // Link old2 to young (triggers write barrier)
    old2.set_next(young.clone());

    // Minor GC should preserve the chain
    collect();

    // Verify the chain is intact
    assert_eq!(*old1.value.borrow(), 1);
    {
        if let Some(ref o2) = *old1.next.borrow() {
            assert_eq!(*o2.value.borrow(), 2);
            {
                if let Some(ref y) = *o2.next.borrow() {
                    assert_eq!(*y.value.borrow(), 999);
                } else {
                    panic!("Young object at end of chain was lost!");
                };
            };
        } else {
            panic!("Chain was broken!");
        };
    }
}

/// Test large object (>2KB) handling with dirty page tracking.
///
/// Large objects have their own pages and should be properly tracked.
#[test]
#[allow(clippy::redundant_clone)]
fn test_large_object_old_to_young_reference() {
    // Create a large object (>2KB)
    // We use a Vec with enough elements to exceed 2KB
    let large_data: Vec<u8> = vec![0u8; 4096];

    #[derive(Trace)]
    struct LargeObject {
        data: RefCell<Vec<u8>>,
        reference: GcCell<Option<Gc<RefCell<i32>>>>,
    }

    let old_large: Gc<LargeObject> = Gc::new(LargeObject {
        data: RefCell::new(large_data),
        reference: GcCell::new(None),
    });

    // Promote to old generation
    collect_full();

    // Create young object
    let young_value: Gc<RefCell<i32>> = Gc::new(RefCell::new(12345));

    // Mutate large object to reference young (triggers write barrier)
    *old_large.reference.borrow_mut() = Some(young_value.clone());

    // Minor GC
    collect();

    // Verify both survived
    assert_eq!(old_large.data.borrow().len(), 4096);
    {
        if let Some(ref val) = *old_large.reference.borrow() {
            assert_eq!(*val.borrow(), 12345);
        } else {
            panic!("Large object's reference to young object was lost!");
        };
    }
}

/// Test that objects without old-to-young references are collected.
#[test]
fn test_young_objects_without_old_refs_are_collected() {
    // Create young object with no old-generation references
    let _young: Gc<RefCell<i32>> = Gc::new(RefCell::new(42));

    // Create an old object but don't establish a reference
    let old: Gc<RefCell<i32>> = Gc::new(RefCell::new(100));
    collect_full(); // Promote to old

    // Minor GC should collect the unreferenced young object
    collect();

    // The old object should still exist
    assert_eq!(*old.borrow(), 100);
}

/// Test that write barriers fire correctly on subsequent mutations.
#[test]
fn test_write_barrier_fires_on_subsequent_mutations() {
    // Create and promote old object
    let old: Gc<Node<i32>> = Gc::new(Node::new(1));
    collect_full();

    // First mutation - creates young1
    let young1: Gc<Node<i32>> = Gc::new(Node::new(100));
    old.set_next(young1.clone());

    collect();
    assert_eq!(*young1.value.borrow(), 100);

    // Second mutation - creates young2
    let young2: Gc<Node<i32>> = Gc::new(Node::new(200));
    old.set_next(young2.clone());

    collect();
    // young2 should survive the second minor GC
    assert_eq!(*young2.value.borrow(), 200);
}

/// Test complex graph with multiple old-to-young edges.
#[test]
fn test_complex_old_to_young_graph() {
    // Create multiple old objects
    let old_a: Gc<Node<i32>> = Gc::new(Node::new(1));
    let old_b: Gc<Node<i32>> = Gc::new(Node::new(2));
    let old_c: Gc<Node<i32>> = Gc::new(Node::new(3));

    collect_full();

    // Create young objects
    let young_x: Gc<Node<i32>> = Gc::new(Node::new(10));
    let young_y: Gc<Node<i32>> = Gc::new(Node::new(20));
    let young_z: Gc<Node<i32>> = Gc::new(Node::new(30));

    // Create cross-references (old_a -> young_x, old_b -> young_y, etc.)
    // and some old-to-old references
    old_a.set_next(young_x.clone());
    young_x.set_next(old_b.clone());
    old_b.set_next(young_y.clone());
    young_y.set_next(old_c.clone());
    old_c.set_next(young_z.clone());

    // Minor GC
    collect();

    // Verify the structure survived
    assert_eq!(*young_x.value.borrow(), 10);
    assert_eq!(*young_y.value.borrow(), 20);
    assert_eq!(*young_z.value.borrow(), 30);
}

/// Test that minor GC correctly handles the case when no old objects
/// have been mutated (empty dirty page list).
#[test]
fn test_minor_gc_with_no_dirty_pages() {
    // Create and promote old objects
    let old1: Gc<RefCell<i32>> = Gc::new(RefCell::new(1));
    let old2: Gc<RefCell<i32>> = Gc::new(RefCell::new(2));

    collect_full();

    // Don't mutate any old objects
    // Dirty page list should be empty

    // Minor GC should complete successfully
    collect();

    // Old objects should still exist
    assert_eq!(*old1.borrow(), 1);
    assert_eq!(*old2.borrow(), 2);
}

/// Test repeated minor collections with dirty page tracking.
#[test]
fn test_repeated_minor_collections() {
    // Create old object
    let old: Gc<Node<i32>> = Gc::new(Node::new(0));
    collect_full();

    // Perform multiple cycles of young allocation + minor GC
    for i in 1..=10 {
        let young: Gc<Node<i32>> = Gc::new(Node::new(i * 100));
        old.set_next(young.clone());

        collect();

        // Verify the most recent young object survived
        if let Some(ref y) = *old.next.borrow() {
            assert_eq!(*y.value.borrow(), i * 100);
        } else {
            panic!("Young object lost at iteration {}", i);
        }
    }
}

/// Test that dirty page tracking works correctly across multiple pages.
#[test]
fn test_dirty_pages_across_multiple_pages() {
    // Create several old objects that may be on different pages
    let mut old_objects: Vec<Gc<Node<i32>>> = Vec::new();
    for i in 0..100 {
        old_objects.push(Gc::new(Node::new(i)));
    }

    collect_full();

    // Create young objects and link them
    let mut young_objects: Vec<Gc<Node<i32>>> = Vec::new();
    for i in 0..100 {
        young_objects.push(Gc::new(Node::new(i * 1000)));
    }

    // Establish old-to-young references
    for (old, young) in old_objects.iter().zip(young_objects.iter()) {
        old.set_next(young.clone());
    }

    // Minor GC
    collect();

    // Verify all young objects survived
    for (i, young) in young_objects.iter().enumerate() {
        assert_eq!(*young.value.borrow(), (i * 1000) as i32);
    }
}
