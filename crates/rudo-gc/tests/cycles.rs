//! Cycle collection tests for rudo-gc.

use rudo_gc::{collect, Gc, Trace};
use std::cell::RefCell;

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
    // Create a -> b -> a cycle
    let a = Gc::new(Node::new(1));
    let b = Gc::new(Node::new(2));

    *a.next.borrow_mut() = Some(Gc::clone(&b));
    *b.next.borrow_mut() = Some(Gc::clone(&a));

    assert_eq!(a.value, 1);
    assert_eq!(b.value, 2);

    // Drop external references
    drop(a);
    drop(b);

    // Force collection - cycle should be detected and freed
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
