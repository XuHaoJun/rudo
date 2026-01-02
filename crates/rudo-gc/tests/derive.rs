//! Tests for the #[derive(Trace)] macro.

use rudo_gc::{collect, Gc, Trace};
use std::cell::RefCell;

/// A simple struct with a Gc field.
#[derive(Trace)]
struct SimpleNode {
    value: i32,
    next: Option<Gc<Self>>,
}

#[test]
fn test_derive_simple_struct() {
    let node = Gc::new(SimpleNode {
        value: 42,
        next: None,
    });
    assert_eq!(node.value, 42);
    assert!(node.next.is_none());
}

#[test]
fn test_derive_with_gc_field() {
    let node1 = Gc::new(SimpleNode {
        value: 1,
        next: None,
    });
    let node2 = Gc::new(SimpleNode {
        value: 2,
        next: Some(Gc::clone(&node1)),
    });
    assert_eq!(node2.value, 2);
    assert!(node2.next.is_some());
    assert_eq!(node2.next.as_ref().unwrap().value, 1);
}

/// Nested struct with multiple Gc fields.
#[derive(Trace)]
struct ComplexNode {
    id: u64,
    name: String,
    left: RefCell<Option<Gc<Self>>>,
    right: RefCell<Option<Gc<Self>>>,
    parent: RefCell<Option<Gc<Self>>>,
}

impl ComplexNode {
    fn new(id: u64, name: &str) -> Self {
        Self {
            id,
            name: name.to_string(),
            left: RefCell::new(None),
            right: RefCell::new(None),
            parent: RefCell::new(None),
        }
    }
}

#[test]
fn test_derive_nested_struct() {
    let root = Gc::new(ComplexNode::new(1, "root"));
    let left = Gc::new(ComplexNode::new(2, "left"));
    let right = Gc::new(ComplexNode::new(3, "right"));

    *root.left.borrow_mut() = Some(Gc::clone(&left));
    *root.right.borrow_mut() = Some(Gc::clone(&right));
    *left.parent.borrow_mut() = Some(Gc::clone(&root));
    *right.parent.borrow_mut() = Some(Gc::clone(&root));

    assert_eq!(root.id, 1);
    assert_eq!(root.left.borrow().as_ref().unwrap().id, 2);
    assert_eq!(root.right.borrow().as_ref().unwrap().id, 3);
}

#[test]
fn test_derive_multiple_gc_fields_cycle() {
    let a = Gc::new(ComplexNode::new(1, "a"));
    let b = Gc::new(ComplexNode::new(2, "b"));
    let c = Gc::new(ComplexNode::new(3, "c"));

    // Create a triangle cycle
    *a.left.borrow_mut() = Some(Gc::clone(&b));
    *b.left.borrow_mut() = Some(Gc::clone(&c));
    *c.left.borrow_mut() = Some(Gc::clone(&a));

    drop(a);
    drop(b);
    drop(c);
    collect();
}

/// Tuple struct.
#[derive(Trace)]
struct TupleNode(i32, Option<Gc<Self>>);

#[test]
fn test_derive_tuple_struct() {
    let node = Gc::new(TupleNode(42, None));
    assert_eq!(node.0, 42);
}

#[test]
fn test_derive_tuple_struct_with_gc() {
    let inner = Gc::new(TupleNode(1, None));
    let outer = Gc::new(TupleNode(2, Some(Gc::clone(&inner))));
    assert_eq!(outer.0, 2);
    assert_eq!(outer.1.as_ref().unwrap().0, 1);
}

/// Unit struct.
#[derive(Trace)]
struct UnitNode;

#[test]
fn test_derive_unit_struct() {
    let _node = Gc::new(UnitNode);
}

/// Enum with variants.
#[derive(Trace)]
enum TreeVariant {
    Leaf(i32),
    Branch { left: Gc<Self>, right: Gc<Self> },
    Empty,
}

#[test]
fn test_derive_enum_unit_variant() {
    let node = Gc::new(TreeVariant::Empty);
    match &*node {
        TreeVariant::Empty => {}
        _ => panic!("Expected Empty"),
    }
}

#[test]
fn test_derive_enum_tuple_variant() {
    let node = Gc::new(TreeVariant::Leaf(42));
    match &*node {
        TreeVariant::Leaf(v) => assert_eq!(*v, 42),
        _ => panic!("Expected Leaf"),
    }
}

#[test]
fn test_derive_enum_struct_variant() {
    let left = Gc::new(TreeVariant::Leaf(1));
    let right = Gc::new(TreeVariant::Leaf(2));
    let branch = Gc::new(TreeVariant::Branch {
        left: Gc::clone(&left),
        right: Gc::clone(&right),
    });

    match &*branch {
        TreeVariant::Branch { left, right } => {
            match &**left {
                TreeVariant::Leaf(v) => assert_eq!(*v, 1),
                _ => panic!("Expected Leaf"),
            }
            match &**right {
                TreeVariant::Leaf(v) => assert_eq!(*v, 2),
                _ => panic!("Expected Leaf"),
            }
        }
        _ => panic!("Expected Branch"),
    }
}

/// Generic struct.
#[derive(Trace)]
struct GenericNode<T: Trace + 'static> {
    value: T,
    next: Option<Gc<Self>>,
}

#[test]
fn test_derive_generic_struct() {
    let node: Gc<GenericNode<i32>> = Gc::new(GenericNode {
        value: 42,
        next: None,
    });
    assert_eq!(node.value, 42);
}

#[test]
fn test_derive_generic_struct_chain() {
    let node1 = Gc::new(GenericNode {
        value: String::from("hello"),
        next: None,
    });
    let node2 = Gc::new(GenericNode {
        value: String::from("world"),
        next: Some(Gc::clone(&node1)),
    });
    assert_eq!(node2.value, "world");
    assert_eq!(node2.next.as_ref().unwrap().value, "hello");
}

/// Struct with multiple type parameters.
#[derive(Trace)]
struct MultiGeneric<K: Trace + 'static, V: Trace + 'static> {
    key: K,
    value: V,
    child: Option<Gc<Self>>,
}

#[test]
fn test_derive_multi_generic() {
    let node: Gc<MultiGeneric<String, i32>> = Gc::new(MultiGeneric {
        key: String::from("test"),
        value: 42,
        child: None,
    });
    assert_eq!(node.key, "test");
    assert_eq!(node.value, 42);
}
