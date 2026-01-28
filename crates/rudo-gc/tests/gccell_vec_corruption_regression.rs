//! Regression test for `GcCell`<Vec<Gc<T>>> corruption during collection.
//!
//! Bug: When calling `collect()`, the GC was incorrectly removing elements from
//! `GcCell`<Vec<Gc<T>>>, causing data loss and crashes.
//!
//! Root cause: Incorrect page address calculation in `GcCell::write_barrier`
//! for large objects, causing dirty bits to be set on wrong objects.

use rudo_gc::{collect, Gc, GcCell, Trace};

#[derive(Trace)]
struct Component {
    id: u64,
    children: GcCell<Vec<Gc<Self>>>,
}

impl Component {
    fn new(id: u64) -> Gc<Self> {
        Gc::new(Self {
            id,
            children: GcCell::new(Vec::new()),
        })
    }

    fn add_child(&self, child: &Gc<Self>) {
        self.children.borrow_mut().push(Gc::clone(child));
    }
}

fn build_tree() -> Gc<Component> {
    let root = Component::new(0);
    root.add_child(&Component::new(1));
    root.add_child(&Component::new(4));
    root.add_child(&Component::new(14));
    root
}

#[test]
fn test_gccell_vec_elements_preserved_during_collect() {
    let root1 = build_tree();
    assert_eq!(root1.children.borrow().len(), 3);

    let _root2 = build_tree();

    collect();

    assert_eq!(root1.children.borrow().len(), 3);
}

#[test]
fn test_gccell_vec_children_accessible_after_collect() {
    let root = build_tree();
    let child_ids: Vec<u64> = root.children.borrow().iter().map(|c| c.id).collect();
    assert_eq!(child_ids, vec![1, 4, 14]);

    let _other = build_tree();
    collect();

    let child_ids_after: Vec<u64> = root.children.borrow().iter().map(|c| c.id).collect();
    assert_eq!(child_ids_after, vec![1, 4, 14]);
}

#[test]
fn test_gccell_vec_with_many_elements() {
    let root = Gc::new(Component {
        id: 0,
        children: GcCell::new(Vec::new()),
    });

    for i in 0..100 {
        root.add_child(&Component::new(i));
    }

    assert_eq!(root.children.borrow().len(), 100);

    let _other = build_tree();
    collect();

    assert_eq!(root.children.borrow().len(), 100);

    let ids: Vec<u64> = root.children.borrow().iter().map(|c| c.id).collect();
    assert_eq!(ids, (0..100).collect::<Vec<_>>());
}

#[test]
fn test_nested_gccell_vec() {
    #[derive(Trace)]
    struct Node {
        id: u64,
        children: GcCell<Vec<Gc<Self>>>,
    }

    impl Node {
        fn new(id: u64) -> Gc<Self> {
            Gc::new(Self {
                id,
                children: GcCell::new(Vec::new()),
            })
        }

        fn add_child(&self, child: &Gc<Self>) {
            self.children.borrow_mut().push(Gc::clone(child));
        }
    }

    let root = Node::new(0);
    let child1 = Node::new(1);
    let child2 = Node::new(2);

    child1.add_child(&Node::new(10));
    child1.add_child(&Node::new(11));

    child2.add_child(&Node::new(20));

    root.add_child(&child1);
    root.add_child(&child2);

    assert_eq!(root.children.borrow().len(), 2);
    assert_eq!(child1.children.borrow().len(), 2);
    assert_eq!(child2.children.borrow().len(), 1);

    let _other = build_tree();
    collect();

    assert_eq!(root.children.borrow().len(), 2);
    assert_eq!(child1.children.borrow().len(), 2);
    assert_eq!(child2.children.borrow().len(), 1);
}
