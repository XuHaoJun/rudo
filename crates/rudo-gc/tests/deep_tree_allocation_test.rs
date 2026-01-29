use rudo_gc::test_util::{clear_test_roots, register_test_root};
use rudo_gc::{collect, Gc, GcCell, Trace};
use std::sync::atomic::AtomicBool;

#[derive(Trace)]
pub struct TestComponent {
    pub id: u64,
    pub children: GcCell<Vec<Gc<Self>>>,
    pub parent: GcCell<Option<Gc<Self>>>,
    pub is_updating: AtomicBool,
}

impl TestComponent {
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
}

fn build_deep_tree() -> Gc<TestComponent> {
    use rudo_gc::Gc;
    let root = Gc::new(TestComponent {
        id: 0,
        children: GcCell::new(Vec::new()),
        parent: GcCell::new(None),
        is_updating: AtomicBool::new(false),
    });
    eprintln!("BUILD: root = {:#x}", Gc::internal_ptr(&root) as usize);
    register_test_root(Gc::internal_ptr(&root).cast::<u8>());

    // Level 1: child1
    let child1 = TestComponent::new(1);
    let grandchild1 = TestComponent::new(2);
    eprintln!("BUILD: child1 = {:#x}", Gc::internal_ptr(&child1) as usize);
    child1.add_child(Gc::clone(&grandchild1));
    root.add_child(Gc::clone(&child1));
    eprintln!(
        "BUILD: after child1, root.children.len = {}",
        root.children.borrow().len()
    );

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
    eprintln!("BUILD: child2 = {:#x}", Gc::internal_ptr(&child2) as usize);

    child2.add_child(Gc::clone(&leaf1));
    child2.add_child(Gc::clone(&leaf2));
    child2.add_child(Gc::clone(&leaf3));
    child2.add_child(Gc::clone(&leaf4));
    child2.add_child(Gc::clone(&leaf5));
    nested.add_child(Gc::clone(&nested_leaf1));
    nested.add_child(Gc::clone(&nested_leaf2));
    child2.add_child(Gc::clone(&nested));
    root.add_child(Gc::clone(&child2));
    eprintln!(
        "BUILD: after child2, root.children.len = {}",
        root.children.borrow().len()
    );

    // Level 1: child3
    let child3 = TestComponent::new(14);
    let leaf = TestComponent::new(15);
    eprintln!("BUILD: child3 = {:#x}", Gc::internal_ptr(&child3) as usize);
    child3.add_child(Gc::clone(&leaf));
    root.add_child(Gc::clone(&child3));
    eprintln!(
        "BUILD: after child3, root.children.len = {}",
        root.children.borrow().len()
    );

    root
}

#[test]
fn test_deep_tree_allocation() {
    clear_test_roots();

    eprintln!("=== Building tree 1 ===");
    let tree1 = build_deep_tree();
    eprintln!("TREE1: {} children", tree1.children.borrow().len());
    assert_eq!(
        tree1.children.borrow().len(),
        3,
        "tree1 should have 3 children"
    );

    eprintln!("=== Building tree 2 ===");
    let tree2 = build_deep_tree();
    eprintln!("TREE2: {} children", tree2.children.borrow().len());
    assert_eq!(
        tree2.children.borrow().len(),
        3,
        "tree2 should have 3 children"
    );

    clear_test_roots();
}

#[test]
fn test_collect_between_deep_trees() {
    clear_test_roots();

    eprintln!("=== Building tree 1 ===");
    let tree1 = build_deep_tree();
    eprintln!("TREE1: {} children", tree1.children.borrow().len());

    eprintln!("=== Calling collect ===");
    collect();

    eprintln!("=== Building tree 2 ===");
    let tree2 = build_deep_tree();
    eprintln!("TREE2: {} children", tree2.children.borrow().len());
    assert_eq!(
        tree2.children.borrow().len(),
        3,
        "tree2 should have 3 children"
    );

    clear_test_roots();
}
