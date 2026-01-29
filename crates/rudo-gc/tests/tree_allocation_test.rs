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

fn build_tree() -> Gc<TestComponent> {
    let root = TestComponent::new(0);
    eprintln!("BUILD: root = {:#x}", Gc::as_ptr(&root) as usize);

    let child1 = TestComponent::new(1);
    eprintln!("BUILD: child1 = {:#x}", Gc::as_ptr(&child1) as usize);
    root.add_child(Gc::clone(&child1));
    eprintln!(
        "BUILD: after child1, root.children.len = {}",
        root.children.borrow().len()
    );

    let child2 = TestComponent::new(2);
    eprintln!("BUILD: child2 = {:#x}", Gc::as_ptr(&child2) as usize);
    root.add_child(Gc::clone(&child2));
    eprintln!(
        "BUILD: after child2, root.children.len = {}",
        root.children.borrow().len()
    );

    let child3 = TestComponent::new(3);
    eprintln!("BUILD: child3 = {:#x}", Gc::as_ptr(&child3) as usize);
    root.add_child(Gc::clone(&child3));
    eprintln!(
        "BUILD: after child3, root.children.len = {}",
        root.children.borrow().len()
    );

    root
}

#[test]
fn test_tree_allocation() {
    eprintln!("=== Building tree 1 ===");
    let tree1 = build_tree();
    eprintln!("TREE1: {} children", tree1.children.borrow().len());
    assert_eq!(tree1.children.borrow().len(), 3);

    eprintln!("=== Building tree 2 ===");
    let tree2 = build_tree();
    eprintln!("TREE2: {} children", tree2.children.borrow().len());
    assert_eq!(tree2.children.borrow().len(), 3);
}

#[test]
fn test_collect_between_trees() {
    use rudo_gc::test_util::{clear_test_roots, register_test_root};
    clear_test_roots();

    eprintln!("=== Building tree 1 ===");
    let tree1 = build_tree();
    register_test_root(Gc::as_ptr(&tree1).cast::<u8>());
    eprintln!("TREE1: {} children", tree1.children.borrow().len());

    eprintln!("=== Calling collect ===");
    collect();

    eprintln!("=== Building tree 2 ===");
    let tree2 = build_tree();
    register_test_root(Gc::as_ptr(&tree2).cast::<u8>());
    eprintln!("TREE2: {} children", tree2.children.borrow().len());

    clear_test_roots();
}
