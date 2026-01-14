//! Tests for `Gc::new_cyclic_weak` self-referential GC support.

#![allow(deprecated)] // For testing deprecated new_cyclic (should migrate to new_cyclic_weak)
#![allow(clippy::doc_markdown, clippy::use_self)] // Test file style preferences

use rudo_gc::{cell::GcCell, collect, Gc, Trace, Weak};

// ============================================================================
// Basic functionality tests
// ============================================================================

#[test]
fn test_new_cyclic_weak_basic() {
    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
        data: i32,
    }

    let node = Gc::new_cyclic_weak(|weak| Node {
        self_ref: GcCell::new(Some(weak)),
        data: 42,
    });

    assert_eq!(node.data, 42);

    let weak = node.self_ref.borrow();
    let weak = weak.as_ref().expect("Weak should exist");
    let upgraded = weak.upgrade().expect("Upgrade should succeed");

    assert!(Gc::ptr_eq(&node, &upgraded));
    assert_eq!(upgraded.data, 42);
}

#[test]
fn test_new_cyclic_weak_multiple_refs() {
    #[derive(Trace)]
    struct MultiRef {
        ref1: GcCell<Option<Weak<MultiRef>>>,
        ref2: GcCell<Option<Weak<MultiRef>>>,
        id: u32,
    }

    let obj = Gc::new_cyclic_weak(|weak| MultiRef {
        ref1: GcCell::new(Some(weak.clone())),
        ref2: GcCell::new(Some(weak)),
        id: 123,
    });

    let r1 = obj.ref1.borrow().as_ref().unwrap().upgrade().unwrap();
    let r2 = obj.ref2.borrow().as_ref().unwrap().upgrade().unwrap();

    assert!(Gc::ptr_eq(&obj, &r1));
    assert!(Gc::ptr_eq(&obj, &r2));
    assert_eq!(obj.id, 123);
}

// ============================================================================
// Complex structure tests
// ============================================================================

#[test]
fn test_new_cyclic_weak_nested() {
    #[derive(Trace)]
    struct Inner {
        self_ref: GcCell<Option<Weak<Inner>>>,
        outer_ref: GcCell<Option<Weak<Outer>>>,
        value: i32,
    }

    #[derive(Trace)]
    struct Outer {
        inner: Gc<Inner>,
        self_ref: GcCell<Option<Weak<Outer>>>,
    }

    let outer = Gc::new_cyclic_weak(|weak_outer| {
        let inner = Gc::new_cyclic_weak(|weak_inner| Inner {
            self_ref: GcCell::new(Some(weak_inner)),
            outer_ref: GcCell::new(Some(weak_outer.clone())),
            value: 100,
        });

        Outer {
            inner,
            self_ref: GcCell::new(Some(weak_outer)),
        }
    });

    assert_eq!(outer.inner.value, 100);

    let inner_weak = outer.inner.self_ref.borrow();
    let inner_upgraded = inner_weak.as_ref().unwrap().upgrade().unwrap();
    assert!(Gc::ptr_eq(&outer.inner, &inner_upgraded));

    let outer_weak = outer.self_ref.borrow();
    let outer_upgraded = outer_weak.as_ref().unwrap().upgrade().unwrap();
    assert!(Gc::ptr_eq(&outer, &outer_upgraded));
}

#[test]
#[cfg_attr(miri, ignore)] // Miri cannot perform stack scanning, so collect() finds no roots
fn test_new_cyclic_weak_with_gc_during_lifetime() {
    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
        data: i32,
    }

    let node = Gc::new_cyclic_weak(|weak| Node {
        self_ref: GcCell::new(Some(weak)),
        data: 42,
    });

    for _ in 0..3 {
        collect();
        let weak = node.self_ref.borrow();
        let upgraded = weak.as_ref().unwrap().upgrade().unwrap();
        assert_eq!(upgraded.data, 42);
    }
}

// ============================================================================
// Internal state tests
// ============================================================================

// ============================================================================
// Doubly-linked list test
// ============================================================================

#[test]
fn test_doubly_linked_list() {
    #[derive(Trace)]
    struct DLNode {
        prev: GcCell<Option<Weak<DLNode>>>,
        next: GcCell<Option<Gc<DLNode>>>,
        value: i32,
    }

    let head = Gc::new(DLNode {
        prev: GcCell::new(None),
        next: GcCell::new(None),
        value: 0,
    });

    let tail = Gc::new(DLNode {
        prev: GcCell::new(Some(Gc::downgrade(&head))),
        next: GcCell::new(None),
        value: 1,
    });

    head.next.borrow_mut().replace(tail.clone());

    assert_eq!(head.next.borrow().as_ref().unwrap().value, 1);
    assert_eq!(
        tail.prev
            .borrow()
            .as_ref()
            .unwrap()
            .upgrade()
            .unwrap()
            .value,
        0
    );
}

// ============================================================================
// Tree structure test
// ============================================================================

#[test]
fn test_tree_parent_child() {
    #[derive(Trace)]
    struct TreeNode {
        parent: GcCell<Option<Weak<TreeNode>>>,
        children: GcCell<Vec<Gc<TreeNode>>>,
        name: String,
    }

    impl TreeNode {
        fn new_root(name: &str) -> Gc<Self> {
            Gc::new(TreeNode {
                parent: GcCell::new(None),
                children: GcCell::new(Vec::new()),
                name: name.to_string(),
            })
        }

        fn add_child(parent: &Gc<Self>, name: &str) -> Gc<Self> {
            let child = Gc::new(TreeNode {
                parent: GcCell::new(Some(Gc::downgrade(parent))),
                children: GcCell::new(Vec::new()),
                name: name.to_string(),
            });
            parent.children.borrow_mut().push(child.clone());
            child
        }
    }

    let root = TreeNode::new_root("root");
    let child1 = TreeNode::add_child(&root, "child1");
    let _child2 = TreeNode::add_child(&root, "child2");
    let _grandchild = TreeNode::add_child(&child1, "grandchild");

    assert_eq!(root.children.borrow().len(), 2);
    assert_eq!(child1.children.borrow().len(), 1);
    assert_eq!(
        child1
            .parent
            .borrow()
            .as_ref()
            .unwrap()
            .upgrade()
            .unwrap()
            .name,
        "root"
    );
}
