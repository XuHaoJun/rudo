//! Test for parent/children cycle double free bug
//!
//! This test verifies that parent/children cyclic references are properly
//! handled without causing double free during the drop process.

use rudo_gc::{collect_full, Gc, GcCell, Trace};

#[derive(Trace)]
struct Parent {
    children: GcCell<Vec<Gc<Child>>>,
    name: i32,
}

#[derive(Trace)]
struct Child {
    parent: GcCell<Option<Gc<Parent>>>,
    name: i32,
}

#[test]
fn test_parent_children_cycle_double_free() {
    let parent = Gc::new(Parent {
        children: GcCell::new(Vec::new()),
        name: 1,
    });

    let child = Gc::new(Child {
        parent: GcCell::new(None),
        name: 2,
    });

    *child.parent.borrow_mut() = Some(Gc::clone(&parent));
    parent.children.borrow_mut().push(Gc::clone(&child));

    drop(parent);
    drop(child);

    collect_full();
}

#[test]
fn test_multiple_children_cycle() {
    let parent = Gc::new(Parent {
        children: GcCell::new(Vec::new()),
        name: 1,
    });

    for i in 0..3 {
        let child = Gc::new(Child {
            parent: GcCell::new(None),
            name: i,
        });
        *child.parent.borrow_mut() = Some(Gc::clone(&parent));
        parent.children.borrow_mut().push(Gc::clone(&child));
    }

    drop(parent);

    collect_full();
}
