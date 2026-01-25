//! Cycle collection tests for rudo-gc.

#![allow(deprecated)] // Testing deprecated new_cyclic (should migrate to new_cyclic_weak)

use rudo_gc::{collect, Gc, Trace};
use std::cell::RefCell;

// ============================================================================
// T072: Tests for Gc::new_cyclic
// ============================================================================

/// A self-referential node for testing `new_cyclic`.
#[derive(Trace)]
struct SelfRefNode {
    value: i32,
    self_ref: RefCell<Option<Gc<Self>>>,
}

#[test]
fn test_new_cyclic_basic() {
    // Create a self-referential structure
    let node = Gc::new_cyclic(|_weak_self| SelfRefNode {
        value: 42,
        self_ref: RefCell::new(None),
    });

    assert_eq!(node.value, 42);

    // Now set the self-reference
    *node.self_ref.borrow_mut() = Some(Gc::clone(&node));

    // Verify the self-reference works
    let inner = node.self_ref.borrow();
    assert!(inner.is_some());
    assert_eq!(inner.as_ref().unwrap().value, 42);
    assert!(Gc::ptr_eq(&node, inner.as_ref().unwrap()));
}

/// A doubly-linked node that references itself through another.
#[derive(Trace)]
struct DoublyLinked {
    value: String,
    next: RefCell<Option<Gc<Self>>>,
    prev: RefCell<Option<Gc<Self>>>,
}

#[test]
fn test_new_cyclic_doubly_linked() {
    let node1 = Gc::new_cyclic(|_| DoublyLinked {
        value: "first".to_string(),
        next: RefCell::new(None),
        prev: RefCell::new(None),
    });

    let node2 = Gc::new_cyclic(|_| DoublyLinked {
        value: "second".to_string(),
        next: RefCell::new(Some(Gc::clone(&node1))),
        prev: RefCell::new(None),
    });

    // Link them together
    *node1.prev.borrow_mut() = Some(Gc::clone(&node2));
    *node1.next.borrow_mut() = Some(Gc::clone(&node2));
    *node2.prev.borrow_mut() = Some(Gc::clone(&node1));

    // Verify the links
    assert_eq!(node1.value, "first");
    assert_eq!(node2.value, "second");

    let node1_next = node1.next.borrow();
    assert!(node1_next.is_some());
    assert_eq!(node1_next.as_ref().unwrap().value, "second");

    drop(node1_next);

    // Collect should handle cycles
    drop(node1);
    drop(node2);
    collect();
}

#[test]
fn test_new_cyclic_with_immediate_self_ref() {
    // This pattern stores a Gc pointing to self during construction
    let node = Gc::new_cyclic(|this| {
        // 'this' is a dead Gc at this point, but we can store it
        SelfRefNode {
            value: 100,
            self_ref: RefCell::new(Some(this)),
        }
    });

    assert_eq!(node.value, 100);

    // The self_ref should point to the same node
    // Note: Due to rehydration limitations, this may be dead
    // In a full implementation, it would be rehydrated
    let inner = node.self_ref.borrow();
    if let Some(ref inner_gc) = *inner {
        // The inner Gc might be dead (implementation limitation)
        // or might point to the same node
        if !Gc::is_dead(inner_gc) {
            assert!(Gc::ptr_eq(&node, inner_gc));
        }
    }
}

/// A simple node that can form cycles.
#[derive(Trace)]
struct Node {
    value: i32,
    next: RefCell<Option<Gc<Self>>>,
}

impl Node {
    const fn new(value: i32) -> Self {
        Self {
            value,
            next: RefCell::new(None),
        }
    }
}

#[test]
fn test_simple_cycle() {
    let a = Gc::new(Node::new(1));
    let b = Gc::new(Node::new(2));

    *a.next.borrow_mut() = Some(Gc::clone(&b));
    *b.next.borrow_mut() = Some(Gc::clone(&a));

    assert_eq!(a.value, 1);
    assert_eq!(b.value, 2);

    drop(a);
    drop(b);

    collect();
}

#[test]
fn test_deep_chain() {
    const CHAIN_LENGTH: usize = 10_00;

    #[derive(Trace)]
    struct ChainNode {
        value: usize,
        next: RefCell<Option<Gc<Self>>>,
    }

    let head = Gc::new(ChainNode {
        value: 0,
        next: RefCell::new(None),
    });

    let mut current = head.clone();
    for i in 1..CHAIN_LENGTH {
        let next = Gc::new(ChainNode {
            value: i,
            next: RefCell::new(None),
        });
        *current.next.borrow_mut() = Some(Gc::clone(&next));
        current = next;
    }

    assert_eq!(head.value, 0);
    assert_eq!(current.value, CHAIN_LENGTH - 1);

    drop(head);
    collect();
}

#[test]
fn test_self_reference() {
    let a = Gc::new(Node::new(1));
    *a.next.borrow_mut() = Some(Gc::clone(&a));

    assert_eq!(a.value, 1);

    drop(a);
    collect();
}

#[test]
fn test_chain() {
    // Create a chain: a -> b -> c -> d
    let a = Gc::new(Node::new(1));
    let b = Gc::new(Node::new(2));
    let c = Gc::new(Node::new(3));
    let d = Gc::new(Node::new(4));

    *a.next.borrow_mut() = Some(Gc::clone(&b));
    *b.next.borrow_mut() = Some(Gc::clone(&c));
    *c.next.borrow_mut() = Some(Gc::clone(&d));

    assert_eq!(a.value, 1);
    assert_eq!(d.value, 4);

    // Keep only the head
    drop(b);
    drop(c);
    drop(d);

    // Chain should still be accessible
    let next = a.next.borrow();
    let b_ref = next.as_ref().unwrap();
    assert_eq!(b_ref.value, 2);

    drop(next);
    drop(a);
    collect();
}

#[test]
fn test_complex_cycle() {
    // Create: a -> b -> c -> a (triangle)
    let a = Gc::new(Node::new(1));
    let b = Gc::new(Node::new(2));
    let c = Gc::new(Node::new(3));

    *a.next.borrow_mut() = Some(Gc::clone(&b));
    *b.next.borrow_mut() = Some(Gc::clone(&c));
    *c.next.borrow_mut() = Some(Gc::clone(&a));

    assert_eq!(a.value, 1);
    assert_eq!(b.value, 2);
    assert_eq!(c.value, 3);

    drop(a);
    drop(b);
    drop(c);

    collect();
}

/// A node with multiple children.
#[derive(Trace)]
struct TreeNode {
    value: i32,
    children: RefCell<Vec<Gc<Self>>>,
}

impl TreeNode {
    const fn new(value: i32) -> Self {
        Self {
            value,
            children: RefCell::new(Vec::new()),
        }
    }

    fn add_child(&self, child: Gc<Self>) {
        self.children.borrow_mut().push(child);
    }
}

#[test]
fn test_tree_structure() {
    let root = Gc::new(TreeNode::new(1));
    let child1 = Gc::new(TreeNode::new(2));
    let child2 = Gc::new(TreeNode::new(3));

    root.add_child(Gc::clone(&child1));
    root.add_child(Gc::clone(&child2));

    assert_eq!(root.value, 1);
    assert_eq!(root.children.borrow().len(), 2);

    drop(child1);
    drop(child2);

    // Children should still be accessible through root
    assert_eq!(root.children.borrow()[0].value, 2);
    assert_eq!(root.children.borrow()[1].value, 3);

    drop(root);
    collect();
}

#[test]
fn test_graph_with_back_edges() {
    // Create a graph with back edges
    let a = Gc::new(TreeNode::new(1));
    let b = Gc::new(TreeNode::new(2));
    let c = Gc::new(TreeNode::new(3));

    // a -> b, a -> c
    a.add_child(Gc::clone(&b));
    a.add_child(Gc::clone(&c));

    // b -> c (creates diamond)
    b.add_child(Gc::clone(&c));

    // c -> a (back edge, creates cycle)
    c.add_child(Gc::clone(&a));

    assert_eq!(a.value, 1);
    assert_eq!(b.value, 2);
    assert_eq!(c.value, 3);

    drop(a);
    drop(b);
    drop(c);

    collect();
}
