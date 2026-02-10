//! Tests for edge cases in Trace derive.

use rudo_gc::{collect, Gc, Trace};
use std::cell::RefCell;

// ============================================================================
// Empty collection tests
// ============================================================================

#[derive(Trace, Debug, PartialEq)]
struct NodeWithEmptyVec {
    id: u64,
    children: Vec<Gc<Self>>,
}

#[test]
fn test_empty_vec_trace() {
    let node = Gc::new(NodeWithEmptyVec {
        id: 1,
        children: Vec::new(),
    });
    assert_eq!(node.id, 1);
    assert!(node.children.is_empty());
    drop(node);
    collect();
}

#[derive(Trace, Debug, PartialEq)]
struct NodeWithEmptyRefCellVec {
    id: u64,
    children: RefCell<Vec<Gc<Self>>>,
}

#[test]
fn test_empty_refcell_vec_trace() {
    let node = Gc::new(NodeWithEmptyRefCellVec {
        id: 1,
        children: RefCell::new(Vec::new()),
    });
    assert!(node.children.borrow().is_empty());
    drop(node);
    collect();
}

// ============================================================================
// Optional Gc<T> tests
// ============================================================================

#[derive(Trace, Debug, PartialEq)]
struct NodeWithOptional {
    id: u64,
    child: Option<Gc<Self>>,
}

#[test]
fn test_optional_gc_none() {
    let node = Gc::new(NodeWithOptional { id: 1, child: None });
    assert_eq!(node.id, 1);
    assert!(node.child.is_none());
    drop(node);
    collect();
}

#[test]
fn test_optional_gc_some() {
    let child = Gc::new(NodeWithOptional { id: 2, child: None });
    let parent = Gc::new(NodeWithOptional {
        id: 1,
        child: Some(Gc::clone(&child)),
    });
    assert_eq!(parent.child.as_ref().unwrap().id, 2);
    drop(parent);
    collect();

    // Drop root variable
    drop(child);
    collect();

    // Object should be dead now
}

#[test]
fn test_optional_gc_drop_child_first() {
    let child = Gc::new(NodeWithOptional { id: 2, child: None });
    let parent = Gc::new(NodeWithOptional {
        id: 1,
        child: Some(Gc::clone(&child)),
    });
    drop(child);
    collect();
    assert!(parent.child.is_some());
    assert_eq!(parent.child.as_ref().unwrap().id, 2);
    drop(parent);
    collect();
}

// ============================================================================
// Nested Option tests
// ============================================================================

#[derive(Trace, Debug, PartialEq)]
#[allow(clippy::option_option)]
struct NodeWithNestedOption {
    id: u64,
    child: Option<Option<Gc<Self>>>,
}

#[test]
fn test_nested_option_none_none() {
    let node = Gc::new(NodeWithNestedOption { id: 1, child: None });
    assert!(node.child.is_none());
    drop(node);
    collect();
}

#[test]
fn test_nested_option_some_none() {
    let node = Gc::new(NodeWithNestedOption {
        id: 1,
        child: Some(None),
    });
    assert_eq!(node.child, Some(None));
    drop(node);
    collect();
}

#[test]
fn test_nested_option_some_some() {
    let child = Gc::new(NodeWithNestedOption { id: 2, child: None });
    let parent = Gc::new(NodeWithNestedOption {
        id: 1,
        child: Some(Some(Gc::clone(&child))),
    });
    assert_eq!(parent.child, Some(Some(Gc::clone(&child))));
    drop(parent);
    collect();

    // Drop root variable
    drop(child);
    collect();

    // Object should be dead now
}

// ============================================================================
// Vec<Gc<T>> with various operations
// ============================================================================

#[derive(Trace, Debug, PartialEq)]
struct Container {
    id: u64,
    children: RefCell<Vec<Gc<Child>>>,
}

#[derive(Trace, Debug, PartialEq)]
struct Child {
    id: u64,
    value: i32,
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unnecessary_cast
)]
fn test_vec_gc_push_and_trace() {
    let container = Gc::new(Container {
        id: 1,
        children: RefCell::new(Vec::new()),
    });

    for i in 0..10 {
        let child = Gc::new(Child {
            id: i as u64,
            value: i as i32,
        });
        container.children.borrow_mut().push(child);
    }

    assert_eq!(container.children.borrow().len(), 10);
    drop(container);
    collect();
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unnecessary_cast
)]
fn test_vec_gc_partial_removal() {
    let container = Gc::new(Container {
        id: 1,
        children: RefCell::new(Vec::new()),
    });

    let children: Vec<Gc<Child>> = (0..10)
        .map(|i| {
            Gc::new(Child {
                id: i as u64,
                value: i as i32,
            })
        })
        .collect();

    for child in &children {
        container.children.borrow_mut().push(Gc::clone(child));
    }

    container.children.borrow_mut().truncate(5);
    assert_eq!(container.children.borrow().len(), 5);
    drop(container);
    collect();

    for (i, child) in children.iter().enumerate() {
        assert!(
            !Gc::is_dead_or_unrooted(child),
            "Child {i} should still be alive"
        );
    }
    drop(children);
    collect();
}

#[test]
fn test_vec_gc_with_shared_elements() {
    let shared_child = Gc::new(Child { id: 99, value: 999 });

    let container = Gc::new(Container {
        id: 1,
        children: RefCell::new(vec![
            Gc::clone(&shared_child),
            Gc::clone(&shared_child),
            Gc::clone(&shared_child),
        ]),
    });

    assert_eq!(container.children.borrow().len(), 3);
    assert!(Gc::ptr_eq(
        &container.children.borrow()[0],
        &container.children.borrow()[1]
    ));

    drop(container);
    collect();
    assert!(!Gc::is_dead_or_unrooted(&shared_child));
    drop(shared_child);
    collect();
}

// ============================================================================
// Vec of pairs tests
// ============================================================================

#[derive(Trace, Debug, PartialEq)]
struct NodeWithVecPair {
    id: u64,
    entries: RefCell<Vec<(Gc<Self>, i32)>>,
}

#[test]
#[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
fn test_vec_pair_gc_trace() {
    let node = Gc::new(NodeWithVecPair {
        id: 1,
        entries: RefCell::new(Vec::new()),
    });

    for i in 0..5 {
        let key = Gc::new(NodeWithVecPair {
            id: 100 + i as u64,
            entries: RefCell::new(Vec::new()),
        });
        node.entries.borrow_mut().push((Gc::clone(&key), i as i32));
    }

    assert_eq!(node.entries.borrow().len(), 5);
    drop(node);
    collect();
}

// ============================================================================
// Recursive optional structures
// ============================================================================

#[derive(Trace, Debug, PartialEq)]
enum ListNode {
    Nil,
    Cons(i32, Box<Self>),
}

#[test]
fn test_nested_enum_with_box() {
    let list = ListNode::Cons(
        1,
        Box::new(ListNode::Cons(
            2,
            Box::new(ListNode::Cons(3, Box::new(ListNode::Nil))),
        )),
    );

    let gc = Gc::new(list);
    match &*gc {
        ListNode::Cons(1, next) => match &**next {
            ListNode::Cons(2, next2) => match &**next2 {
                ListNode::Cons(3, next3) => {
                    assert!(matches!(&**next3, ListNode::Nil));
                }
                _ => panic!("Expected 3"),
            },
            _ => panic!("Expected 2"),
        },
        _ => panic!("Expected 1"),
    }
    drop(gc);
    collect();
}

// ============================================================================
// GcCell with nested Gc
// ============================================================================

#[derive(Trace, Debug, PartialEq)]
struct Component {
    id: u64,
    state: RefCell<Option<Box<Gc<Self>>>>,
}

#[test]
fn test_nested_gc_in_box() {
    let inner = Gc::new(Component {
        id: 2,
        state: RefCell::new(None),
    });

    let outer = Gc::new(Component {
        id: 1,
        state: RefCell::new(Some(Box::new(Gc::clone(&inner)))),
    });

    {
        let retrieved = outer.state.borrow();
        let inner_ref = retrieved.as_ref().unwrap();
        assert_eq!(inner_ref.id, 2);
    }

    drop(outer);
    collect();
    assert!(!Gc::is_dead_or_unrooted(&inner));
    drop(inner);
    collect();
}

#[test]
fn test_gc_inside_gc() {
    #[derive(Trace, Debug)]
    struct Inner {
        value: i32,
    }

    #[derive(Trace, Debug)]
    struct Outer {
        inner: Gc<Inner>,
        value: i32,
    }

    let inner = Gc::new(Inner { value: 42 });
    let outer = Gc::new(Outer {
        inner: Gc::clone(&inner),
        value: 100,
    });

    assert_eq!(outer.inner.value, 42);
    assert_eq!(outer.value, 100);

    let inner_ptr = Gc::as_ptr(&inner) as usize;
    let outer_ptr = Gc::as_ptr(&outer) as usize;
    assert_ne!(
        inner_ptr, outer_ptr,
        "Inner and Outer should be at different addresses"
    );

    drop(outer);
    collect();
    assert!(!Gc::is_dead_or_unrooted(&inner));
    drop(inner);
    collect();
}

// ============================================================================
// Array type tests
// ============================================================================

#[derive(Trace, Debug)]
#[repr(C)]
struct EmptyArray {
    id: u64,
    arr: [Gc<Self>; 0],
}

#[test]
fn test_empty_array_trace() {
    let node = Gc::new(EmptyArray { id: 1, arr: [] });
    assert_eq!(node.id, 1);
    drop(node);
    collect();
}
