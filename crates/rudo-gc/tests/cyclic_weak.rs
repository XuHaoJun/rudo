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
// Deep tree structure test - Reproduces cross-heap reference issue
// ============================================================================

#[test]
#[cfg_attr(miri, ignore)] // May take too long for Miri
fn test_deep_tree_with_gccell_vec_gc() {
    use rudo_gc::test_util::register_test_root;

    #[derive(Trace)]
    struct Component {
        id: u64,
        children: GcCell<Vec<Gc<Component>>>,
        parent: GcCell<Option<Weak<Component>>>,
        is_updating: std::sync::atomic::AtomicBool,
    }

    impl Component {
        fn new(id: u64) -> Gc<Self> {
            Gc::new(Self {
                id,
                children: GcCell::new(Vec::new()),
                parent: GcCell::new(None),
                is_updating: std::sync::atomic::AtomicBool::new(false),
            })
        }

        fn add_child(&self, child: Gc<Self>) {
            if std::ptr::eq(&*child, &*self) {
                return;
            }
            self.children.borrow_mut().push(Gc::clone(&child));
        }

        fn update(&self) {
            let was_updating = self
                .is_updating
                .swap(true, std::sync::atomic::Ordering::SeqCst);
            if was_updating {
                return;
            }
            for child in self.children.borrow().iter() {
                let _ = child.id;
                child.update();
            }
            self.is_updating
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    let root = Component::new(0);

    let child1 = Component::new(1);
    let grandchild1 = Component::new(2);
    child1.add_child(Gc::clone(&grandchild1));
    root.add_child(Gc::clone(&child1));

    let child2 = Component::new(4);
    let leaf1 = Component::new(5);
    let leaf2 = Component::new(6);
    let leaf3 = Component::new(7);
    let leaf4 = Component::new(8);
    let leaf5 = Component::new(9);
    let nested = Component::new(11);
    let nested_leaf1 = Component::new(12);
    let nested_leaf2 = Component::new(13);

    child2.add_child(Gc::clone(&leaf1));
    child2.add_child(Gc::clone(&leaf2));
    child2.add_child(Gc::clone(&leaf3));
    child2.add_child(Gc::clone(&leaf4));
    child2.add_child(Gc::clone(&leaf5));
    nested.add_child(Gc::clone(&nested_leaf1));
    nested.add_child(Gc::clone(&nested_leaf2));
    child2.add_child(Gc::clone(&nested));
    root.add_child(Gc::clone(&child2));

    let child3 = Component::new(14);
    let leaf = Component::new(15);
    child3.add_child(Gc::clone(&leaf));
    root.add_child(Gc::clone(&child3));

    register_test_root(Gc::as_ptr(&root) as *const u8);

    assert_eq!(root.children.borrow().len(), 3);

    let child2_ref = Gc::clone(&root.children.borrow()[1]);
    assert_eq!(child2_ref.id, 4);
    assert_eq!(child2_ref.children.borrow().len(), 6);

    let first_child = Gc::clone(&child2_ref.children.borrow()[0]);
    assert_eq!(first_child.id, 5);

    collect();

    assert_eq!(root.children.borrow().len(), 3);
    let child2_after_gc = Gc::clone(&root.children.borrow()[1]);
    assert_eq!(child2_after_gc.children.borrow().len(), 6);

    let first_child_after_gc = Gc::clone(&child2_after_gc.children.borrow()[0]);
    assert_eq!(first_child_after_gc.id, 5);

    root.update();

    rudo_gc::test_util::clear_test_roots();
    drop(root);
    collect();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_gccell_vec_many_children_survives_gc() {
    #[derive(Trace)]
    struct Node {
        children: GcCell<Vec<Gc<Node>>>,
    }

    let parent = Gc::new(Node {
        children: GcCell::new(Vec::new()),
    });

    for i in 0..100 {
        let child = Gc::new(Node {
            children: GcCell::new(Vec::new()),
        });
        parent.children.borrow_mut().push(Gc::clone(&child));

        if i < 10 {
            for j in 0..5 {
                let grandchild = Gc::new(Node {
                    children: GcCell::new(Vec::new()),
                });
                child.children.borrow_mut().push(Gc::clone(&grandchild));
            }
        }
    }

    assert_eq!(parent.children.borrow().len(), 100);

    collect();

    assert_eq!(parent.children.borrow().len(), 100);

    for (i, child) in parent.children.borrow().iter().enumerate() {
        let _ = i;
        if i < 10 {
            assert_eq!(child.children.borrow().len(), 5);
        }
    }

    drop(parent);
    collect();
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
