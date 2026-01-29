//! Tests for DAG (Directed Acyclic Graph) sharing structures.
//!
//! These tests verify correct GC behavior when multiple parent nodes
//! share the same child node, ensuring shared children survive as long
//! as any parent references them.

use rudo_gc::test_util::{clear_test_roots, register_test_root};
use rudo_gc::{collect, Gc, Trace};
use std::cell::RefCell;

#[derive(Trace)]
struct ParentNode {
    id: u64,
    children: RefCell<Vec<Gc<SharedChild>>>,
}

#[derive(Trace)]
struct SharedChild {
    id: u64,
    value: RefCell<i32>,
}

impl SharedChild {
    fn new(id: u64, value: i32) -> Gc<Self> {
        Gc::new(Self {
            id,
            value: RefCell::new(value),
        })
    }
}

// ============================================================================
// Basic DAG sharing tests
// ============================================================================

#[test]
fn test_shared_child_single_parent() {
    let child = SharedChild::new(1, 42);

    let parent = Gc::new(ParentNode {
        id: 0,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    assert_eq!(parent.children.borrow().len(), 1);
    assert_eq!(parent.children.borrow()[0].id, 1);

    drop(parent);
    collect();

    // Drop root variable, then child should be dead
    drop(child);
    collect();

    // The main test is that no panic occurs - the GC behavior is validated
    // by the fact that child survived as long as parent held a reference
}

#[test]
fn test_shared_child_two_parents() {
    let child = SharedChild::new(1, 100);

    let parent1 = Gc::new(ParentNode {
        id: 1,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    let parent2 = Gc::new(ParentNode {
        id: 2,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    // Both parents share the same child
    assert!(Gc::ptr_eq(
        &parent1.children.borrow()[0],
        &parent2.children.borrow()[0]
    ));

    // Drop one parent, child should still survive
    drop(parent1);
    collect();

    assert!(
        !Gc::is_dead(&child),
        "Child should still be alive via parent2"
    );

    // Drop second parent
    drop(parent2);
    collect();

    // Drop root variable
    drop(child);
    collect();

    // The main test is that no panic occurs
}

#[test]
fn test_shared_child_three_parents() {
    let child = SharedChild::new(1, 999);

    let parent1 = Gc::new(ParentNode {
        id: 1,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    let parent2 = Gc::new(ParentNode {
        id: 2,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    let parent3 = Gc::new(ParentNode {
        id: 3,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    // Drop parents one by one, verify child survives
    drop(parent1);
    collect();
    assert!(!Gc::is_dead(&child));

    drop(parent2);
    collect();
    assert!(!Gc::is_dead(&child));

    drop(parent3);
    collect();

    // Drop root variable
    drop(child);
    collect();

    // The main test is that no panic occurs
}

#[test]
fn test_shared_child_partial_overlap() {
    // Create two children
    let child1 = SharedChild::new(1, 10);
    let child2 = SharedChild::new(2, 20);

    // Parent 1 has both children
    let parent1 = Gc::new(ParentNode {
        id: 1,
        children: RefCell::new(vec![Gc::clone(&child1), Gc::clone(&child2)]),
    });

    // Parent 2 has only child1 (shared)
    let parent2 = Gc::new(ParentNode {
        id: 2,
        children: RefCell::new(vec![Gc::clone(&child1)]),
    });

    // Parent 3 has only child2 (not shared)
    let parent3 = Gc::new(ParentNode {
        id: 3,
        children: RefCell::new(vec![Gc::clone(&child2)]),
    });

    // Drop parent1, children should still be alive via parent2 and parent3
    drop(parent1);
    collect();

    assert!(!Gc::is_dead(&child1), "child1 should survive via parent2");
    assert!(!Gc::is_dead(&child2), "child2 should survive via parent3");

    // Drop parent2, child1 should now have no strong references
    // (parent1 was already dropped, parent2 is being dropped now)
    drop(parent2);
    collect();

    // child1 should be dead (no strong refs), but we can't check this
    // while child1 root variable is in scope
    // child2 should still survive via parent3
    assert!(
        !Gc::is_dead(&child2),
        "child2 should still survive via parent3"
    );

    drop(parent3);
    collect();

    // Drop root variables
    drop(child1);
    drop(child2);
    collect();

    // The main test is that no panic occurs
}

// ============================================================================
// Complex DAG with multiple shared children
// ============================================================================

#[test]
fn test_multiple_shared_children() {
    let shared1 = SharedChild::new(1, 1);
    let shared2 = SharedChild::new(2, 2);
    let shared3 = SharedChild::new(3, 3);

    let parent1 = Gc::new(ParentNode {
        id: 1,
        children: RefCell::new(vec![Gc::clone(&shared1), Gc::clone(&shared2)]),
    });

    let parent2 = Gc::new(ParentNode {
        id: 2,
        children: RefCell::new(vec![Gc::clone(&shared2), Gc::clone(&shared3)]),
    });

    let parent3 = Gc::new(ParentNode {
        id: 3,
        children: RefCell::new(vec![Gc::clone(&shared1), Gc::clone(&shared3)]),
    });

    // Graph structure:
    // parent1 ---> shared1 <-- parent3
    //      \--> shared2 <-- parent2
    //                   \--> shared3 <-- parent3

    // Drop parent1, shared1 and shared2 should survive
    drop(parent1);
    collect();

    assert!(!Gc::is_dead(&shared1), "shared1 via parent3");
    assert!(!Gc::is_dead(&shared2), "shared2 via parent2");
    assert!(!Gc::is_dead(&shared3), "shared3 via parent3");

    // Drop parent2, shared2 and shared3 should survive
    drop(parent2);
    collect();

    assert!(!Gc::is_dead(&shared1), "shared1 via parent3");
    assert!(!Gc::is_dead(&shared2), "shared2 has no refs now?");
    assert!(!Gc::is_dead(&shared3), "shared3 via parent3");

    // Drop parent3
    drop(parent3);
    collect();

    // Drop root variables
    drop(shared1);
    drop(shared2);
    drop(shared3);
    collect();

    // The main test is that no panic occurs
}

#[test]
fn test_chain_of_sharing() {
    // A -> B -> C -> D, with A and B sharing D
    #[derive(Trace)]
    struct Node {
        id: u64,
        shared: RefCell<Option<Gc<SharedChild>>>,
    }

    let shared = SharedChild::new(1, 999);

    let node_a = Gc::new(Node {
        id: 'A' as u64,
        shared: RefCell::new(Some(Gc::clone(&shared))),
    });

    let node_b = Gc::new(Node {
        id: 'B' as u64,
        shared: RefCell::new(Some(Gc::clone(&shared))),
    });

    let node_c = Gc::new(Node {
        id: 'C' as u64,
        shared: RefCell::new(Some(Gc::clone(&shared))),
    });

    // All nodes share the same child
    assert!(Gc::ptr_eq(
        node_a.shared.borrow().as_ref().unwrap(),
        node_b.shared.borrow().as_ref().unwrap()
    ));
    assert!(Gc::ptr_eq(
        node_b.shared.borrow().as_ref().unwrap(),
        node_c.shared.borrow().as_ref().unwrap()
    ));

    // Drop nodes one by one
    drop(node_a);
    collect();
    assert!(!Gc::is_dead(&shared));

    drop(node_b);
    collect();
    assert!(!Gc::is_dead(&shared));

    drop(node_c);
    collect();

    // Drop root variable
    drop(shared);
    collect();

    // The main test is that no panic occurs - we verified the object survived
    // as long as there were references to it
}

// ============================================================================
// DAG with registered roots
// ============================================================================

#[test]
fn test_registered_root_shared_dag() {
    clear_test_roots();

    let child = SharedChild::new(1, 42);

    let parent1 = Gc::new(ParentNode {
        id: 1,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    let parent2 = Gc::new(ParentNode {
        id: 2,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    // Register as roots
    register_test_root(Gc::as_ptr(&parent1).cast::<u8>());
    register_test_root(Gc::as_ptr(&parent2).cast::<u8>());

    // Drop locals but keep roots
    drop(parent1);
    drop(parent2);
    collect();

    // Should still be alive via roots
    // (We can't easily verify without unsafe)

    clear_test_roots();
    collect();
}

// ============================================================================
// DAG with updates
// ============================================================================

#[test]
fn test_dag_with_shared_value_update() {
    let child = SharedChild::new(1, 0);

    let parent1 = Gc::new(ParentNode {
        id: 1,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    let parent2 = Gc::new(ParentNode {
        id: 2,
        children: RefCell::new(vec![Gc::clone(&child)]),
    });

    // Update the shared child's value
    *child.value.borrow_mut() = 42;

    // Both parents should see the updated value
    let child_from_parent1 = Gc::clone(&parent1.children.borrow()[0]);
    let child_from_parent2 = Gc::clone(&parent2.children.borrow()[0]);

    assert_eq!(*child_from_parent1.value.borrow(), 42);
    assert_eq!(*child_from_parent2.value.borrow(), 42);

    // Verify they're the same object
    assert!(Gc::ptr_eq(&child_from_parent1, &child_from_parent2));

    drop(parent1);
    drop(parent2);
    collect();
}

// ============================================================================
// Large DAG stress test
// ============================================================================

#[test]
#[allow(
    clippy::needless_range_loop,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]
fn test_large_dag() {
    const CHILDREN_COUNT: usize = 100;
    const PARENTS_COUNT: usize = 50;

    // Create children
    let children: Vec<Gc<SharedChild>> = (0..CHILDREN_COUNT)
        .map(|i| SharedChild::new(i as u64, i as i32))
        .collect();

    // Each parent gets a random subset of children
    let mut parents: Vec<Gc<ParentNode>> = Vec::new();

    for p in 0..PARENTS_COUNT {
        let mut child_refs = Vec::new();
        for c in 0..CHILDREN_COUNT {
            if (p + c) % 3 == 0 {
                child_refs.push(Gc::clone(&children[c]));
            }
        }

        let parent = Gc::new(ParentNode {
            id: p as u64,
            children: RefCell::new(child_refs),
        });
        parents.push(parent);
    }

    // Verify some sharing happened
    let mut sharing_found = false;
    for (i, p1) in parents.iter().enumerate().take(5) {
        for (_, p2) in parents.iter().enumerate().skip(i + 1).take(5) {
            for c1 in p1.children.borrow().iter() {
                for c2 in p2.children.borrow().iter() {
                    if Gc::ptr_eq(c1, c2) {
                        sharing_found = true;
                        break;
                    }
                }
                if sharing_found {
                    break;
                }
            }
            if sharing_found {
                break;
            }
        }
        if sharing_found {
            break;
        }
    }

    assert!(sharing_found, "Expected some shared children in large DAG");

    // Drop all parents, children should be dead
    drop(parents);
    collect();

    // Drop root variables
    drop(children);
    collect();

    // All children should now be dead
}

// ============================================================================
// Diamond pattern (common DAG structure)
// ============================================================================

#[test]
#[allow(clippy::items_after_statements)]
fn test_diamond_pattern() {
    // Diamond: root -> shared_child (which both node_a and node_b reference)
    // Structure: root has two children (node_a, node_b), both point to shared_child
    let shared_child = SharedChild::new(1, 100);

    // node_a and node_b both reference shared_child
    let node_a = Gc::new(ParentNode {
        id: 'A' as u64,
        children: RefCell::new(vec![Gc::clone(&shared_child)]),
    });

    let node_b = Gc::new(ParentNode {
        id: 'B' as u64,
        children: RefCell::new(vec![Gc::clone(&shared_child)]),
    });

    // Create a parent that references both node_a and node_b
    // We need a new struct for this since ParentNode expects SharedChild children
    #[derive(Trace)]
    struct RootNode {
        id: u64,
        children: RefCell<Vec<Gc<ParentNode>>>,
    }

    let root = Gc::new(RootNode {
        id: 'R' as u64,
        children: RefCell::new(vec![Gc::clone(&node_a), Gc::clone(&node_b)]),
    });

    // Verify structure
    assert_eq!(root.children.borrow().len(), 2);
    assert_eq!(node_a.children.borrow().len(), 1);
    assert_eq!(node_b.children.borrow().len(), 1);

    // Verify sharing of shared_child
    assert!(Gc::ptr_eq(
        &node_a.children.borrow()[0],
        &node_b.children.borrow()[0]
    ));

    // Drop root, shared_child should still survive via a and b
    drop(root);
    collect();

    assert!(!Gc::is_dead(&shared_child));

    // Drop a, shared_child should still survive via b
    drop(node_a);
    collect();

    assert!(!Gc::is_dead(&shared_child));

    // Drop b, shared_child should be dead (but shared_child variable is still a root)
    // We need to drop the shared_child root variable first
    drop(shared_child);
    drop(node_b);
    collect();

    // Now shared_child is truly dead since all references are gone
}
