//! Test for deep tree structure corruption with `GcCell`<Vec<Gc<T>>>
//! This reproduces the issue seen in Rvue's layout example

use rudo_gc::{collect, Gc, GcCell, Trace};
use std::sync::atomic::{AtomicBool, Ordering};

/// Simplified Component structure matching Rvue's Component
#[derive(Trace)]
pub struct TestComponent {
    pub id: u64,
    pub children: GcCell<Vec<Gc<Self>>>,
    pub parent: GcCell<Option<Gc<Self>>>, // NOT traced (avoids cycles)
    pub is_updating: AtomicBool,
}

impl TestComponent {
    #[must_use]
    pub fn new(id: u64) -> Gc<Self> {
        Gc::new(Self {
            id,
            children: GcCell::new(Vec::new()),
            parent: GcCell::new(None),
            is_updating: AtomicBool::new(false),
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn add_child(&self, child: Gc<Self>) {
        if std::ptr::eq(&raw const *child, self) {
            return;
        }
        self.children.borrow_mut().push(Gc::clone(&child));
    }

    pub fn update(&self) {
        let was_updating = self.is_updating.swap(true, Ordering::SeqCst);
        if was_updating {
            return;
        }
        for child in self.children.borrow().iter() {
            let _ = child.id;
            child.update();
        }
        self.is_updating.store(false, Ordering::SeqCst);
    }
}

/// Effect structure (simplified from Rvue)
#[derive(Trace)]
pub struct TestEffect {
    pub is_dirty: AtomicBool,
}

impl TestEffect {
    #[must_use]
    pub fn new() -> Gc<Self> {
        Gc::new(Self {
            is_dirty: AtomicBool::new(true),
        })
    }
}

/// `TestViewStruct` like in Rvue
#[derive(Trace)]
pub struct TestViewStruct {
    pub root_component: Gc<TestComponent>,
    pub effects: GcCell<Vec<Gc<TestEffect>>>,
}

/// `AppState` like in Rvue with multiple roots
#[derive(Trace)]
pub struct TestAppState {
    pub view: GcCell<Option<TestViewStruct>>,
    pub scene: GcCell<Vec<Gc<TestComponent>>>,
    pub active_path: GcCell<Vec<Gc<TestComponent>>>,
}

impl TestAppState {
    pub fn new() -> Gc<Self> {
        Gc::new(Self {
            view: GcCell::new(None),
            scene: GcCell::new(Vec::new()),
            active_path: GcCell::new(Vec::new()),
        })
    }

    pub fn set_view(&self, view: TestViewStruct) {
        *self.view.borrow_mut() = Some(view);
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn add_scene_root(&self, component: Gc<TestComponent>) {
        self.scene.borrow_mut().push(Gc::clone(&component));
    }
}

/// Build a deep tree similar to Rvue's layout example
fn build_deep_tree() -> Gc<TestComponent> {
    let root = TestComponent::new(0);

    // Level 1: child1
    let child1 = TestComponent::new(1);
    let grandchild1 = TestComponent::new(2);
    child1.add_child(Gc::clone(&grandchild1));
    root.add_child(Gc::clone(&child1));

    // Level 1: child2 (with 6 children)
    let child2 = TestComponent::new(4);
    let leaf1 = TestComponent::new(5);
    let leaf2 = TestComponent::new(6);
    let leaf3 = TestComponent::new(7);
    let leaf4 = TestComponent::new(8);
    let leaf5 = TestComponent::new(9);
    let nested = TestComponent::new(11);
    let nested_leaf1 = TestComponent::new(12);
    let nested_leaf2 = TestComponent::new(13);

    child2.add_child(Gc::clone(&leaf1));
    child2.add_child(Gc::clone(&leaf2));
    child2.add_child(Gc::clone(&leaf3));
    child2.add_child(Gc::clone(&leaf4));
    child2.add_child(Gc::clone(&leaf5));
    nested.add_child(Gc::clone(&nested_leaf1));
    nested.add_child(Gc::clone(&nested_leaf2));
    child2.add_child(Gc::clone(&nested));
    root.add_child(Gc::clone(&child2));

    // Level 1: child3
    let child3 = TestComponent::new(14);
    let leaf = TestComponent::new(15);
    child3.add_child(Gc::clone(&leaf));
    root.add_child(Gc::clone(&child3));

    root
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_deep_tree_update_corruption() {
    let root = build_deep_tree();

    // Verify structure
    assert_eq!(root.children.borrow().len(), 3);
    let child2 = Gc::clone(&root.children.borrow()[1]);
    assert_eq!(child2.id, 4);
    assert_eq!(child2.children.borrow().len(), 6);

    // Trigger GC
    collect();

    // Verify after GC
    assert_eq!(root.children.borrow().len(), 3);
    let child2_after_gc = Gc::clone(&root.children.borrow()[1]);
    assert_eq!(child2_after_gc.children.borrow().len(), 6);

    // Access first child
    let first_child = Gc::clone(&child2_after_gc.children.borrow()[0]);
    assert_eq!(first_child.id, 5);

    // Update
    root.update();

    drop(root);
    collect();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_rvue_like_usage() {
    let root = build_deep_tree();
    let effect = TestEffect::new();

    // Create ViewStruct like Rvue
    let view = TestViewStruct {
        root_component: Gc::clone(&root),
        effects: GcCell::new(vec![Gc::clone(&effect)]),
    };

    // Create AppState like Rvue
    let app_state = TestAppState::new();
    app_state.set_view(view);
    app_state.add_scene_root(Gc::clone(&root));

    // Simulate event loop iterations with GC
    for _ in 0..3 {
        root.update();
        collect();
    }

    // Verify tree is still valid
    assert_eq!(root.children.borrow().len(), 3);
    let child2 = Gc::clone(&root.children.borrow()[1]);
    assert_eq!(child2.children.borrow().len(), 6);

    for child in child2.children.borrow().iter() {
        let _ = child.id;
    }

    drop(app_state);
    collect();
}

#[test]
fn test_multiple_gc_roots() {
    // Build two separate trees
    let root1 = build_deep_tree();
    let root1_children_count = root1.children.borrow().len();
    assert!(root1_children_count == 3, "root1 should have 3 children");

    let root2 = build_deep_tree();
    let root2_children_count = root2.children.borrow().len();
    assert!(root2_children_count == 3, "root2 should have 3 children");

    // Check counts immediately
    assert!(root1_children_count == 3, "root1 should have 3 children");

    // Create app state with multiple roots
    let app_state = TestAppState::new();
    app_state.add_scene_root(Gc::clone(&root1));
    app_state.add_scene_root(Gc::clone(&root2));

    // Collect - both trees should be preserved
    assert!(root1_children_count == 3, "root1 should have 3 children");
    collect();
    let root1_after_gc = root1.children.borrow().len();
    assert!(root1_after_gc == 3, "root1 should have 3 children after GC");

    // Both roots should still have all children
    assert_eq!(root1.children.borrow().len(), 3, "root1 after GC");
    assert_eq!(root2.children.borrow().len(), 3, "root2 after GC");

    // Update both
    root1.update();
    root2.update();

    // Collect again
    collect();

    // Verify still valid
    let child2 = Gc::clone(&root1.children.borrow()[1]);
    assert_eq!(child2.children.borrow().len(), 6, "child2 children count");

    // Access all children to verify they're not corrupted
    for (i, child) in child2.children.borrow().iter().enumerate() {
        assert!(
            child.id >= 5 && child.id <= 13,
            "child {} has invalid id {}",
            i,
            child.id
        );
    }

    drop(app_state);
    collect();
}
