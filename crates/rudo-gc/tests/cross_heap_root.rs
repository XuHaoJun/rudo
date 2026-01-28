#![allow(
    clippy::needless_pass_by_value,
    clippy::borrow_as_ptr,
    clippy::borrow_deref_ref
)] // Test file style preferences

//! Tests for deep tree structure corruption in `GcCell`<Vec<Gc<T>>>.
//!
//! This test module reproduces the bug where deeply nested component trees
//! suffer memory corruption when garbage collection runs.
//!
//! Issue: `GcCell`<Vec<Gc<T>>> structures cause memory corruption during GC
//! when the tree structure is deeply nested (6+ levels).

use rudo_gc::test_util::register_test_root;
use rudo_gc::{collect, Gc, GcCell, Trace};
use std::sync::atomic::AtomicBool;

#[derive(Trace)]
struct Component {
    id: u64,
    children: GcCell<Vec<Gc<Self>>>,
    parent: GcCell<Option<Gc<Self>>>,
    is_updating: AtomicBool,
}

impl Component {
    fn new(id: u64) -> Gc<Self> {
        Gc::new(Self {
            id,
            children: GcCell::new(Vec::new()),
            parent: GcCell::new(None),
            is_updating: AtomicBool::new(false),
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

fn build_deep_tree() -> Gc<Component> {
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

    root
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_deep_tree_update_corruption() {
    let root = build_deep_tree();

    register_test_root(rudo_gc::Gc::internal_ptr(&root));

    assert_eq!(root.children.borrow().len(), 3);

    let child2 = Gc::clone(&root.children.borrow()[1]);
    assert_eq!(child2.id, 4);
    assert_eq!(child2.children.borrow().len(), 6);

    let first_child = Gc::clone(&child2.children.borrow()[0]);
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
fn test_gccell_vec_gc_trace() {
    #[derive(Trace)]
    struct Node {
        children: GcCell<Vec<Gc<Self>>>,
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
            for _j in 0..5 {
                let grandchild = Gc::new(Node {
                    children: GcCell::new(Vec::new()),
                });
                child.children.borrow_mut().push(Gc::clone(&grandchild));
            }
        }
    }

    register_test_root(rudo_gc::Gc::internal_ptr(&parent));

    assert_eq!(parent.children.borrow().len(), 100);

    collect();

    assert_eq!(parent.children.borrow().len(), 100);

    for (i, child) in parent.children.borrow().iter().enumerate() {
        let _ = i;
        if i < 10 {
            assert_eq!(child.children.borrow().len(), 5);
        }
    }

    rudo_gc::test_util::clear_test_roots();
    drop(parent);
    collect();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_registered_root_deep_tree() {
    let root = build_deep_tree();

    register_test_root(rudo_gc::Gc::internal_ptr(&root));

    assert_eq!(root.children.borrow().len(), 3);

    collect();

    assert_eq!(root.children.borrow().len(), 3);
    let child2 = Gc::clone(&root.children.borrow()[1]);
    assert_eq!(child2.children.borrow().len(), 6);

    root.update();

    rudo_gc::test_util::clear_test_roots();
    drop(root);
    collect();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_many_gccollections() {
    let root = build_deep_tree();

    register_test_root(rudo_gc::Gc::internal_ptr(&root));

    for _ in 0..10 {
        collect();
        let child2 = Gc::clone(&root.children.borrow()[1]);
        let first_child = Gc::clone(&child2.children.borrow()[0]);
        assert_eq!(first_child.id, 5);
    }

    rudo_gc::test_util::clear_test_roots();
    drop(root);
    collect();
}
