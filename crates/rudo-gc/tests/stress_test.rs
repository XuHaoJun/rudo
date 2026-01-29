//! Stress tests for garbage collection.

use rudo_gc::test_util::{clear_test_roots, register_test_root};
use rudo_gc::{collect, Gc, GcCell, Trace};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Trace)]
struct ChainNode {
    value: usize,
    next: GcCell<Option<Gc<Self>>>,
}

impl ChainNode {
    fn new(value: usize) -> Gc<Self> {
        Gc::new(Self {
            value,
            next: GcCell::new(None),
        })
    }
}

impl ChainNode {
    fn append(&self, next: Gc<Self>) {
        self.next.borrow_mut().replace(next);
    }
}

#[test]
fn test_deep_chain_1000() {
    const DEPTH: usize = 1_000;
    let head = ChainNode::new(0);
    let mut current = head.clone();
    for i in 1..DEPTH {
        let next = ChainNode::new(i);
        current.append(Gc::clone(&next));
        current = next;
    }
    assert_eq!(head.value, 0);
    assert_eq!(current.value, DEPTH - 1);
    drop(head);
    collect();
}

#[test]
fn test_deep_chain_traverse_after_gc() {
    const DEPTH: usize = 1_000;
    let head = ChainNode::new(0);
    let mut current = head.clone();
    for i in 1..DEPTH {
        let next = ChainNode::new(i);
        current.append(Gc::clone(&next));
        current = next;
    }
    for _ in 0..3 {
        collect();
    }
    let mut count = 0;
    let mut node_opt = Some(head.clone());
    while let Some(node) = node_opt {
        count += 1;
        let next_ref = node.next.borrow();
        node_opt = next_ref.as_ref().map(Gc::clone);
    }
    assert_eq!(count, DEPTH);
    drop(head);
    collect();
}

#[test]
fn test_deep_chain_with_registered_root() {
    const DEPTH: usize = 1_000;
    let head = ChainNode::new(0);
    register_test_root(Gc::as_ptr(&head).cast::<u8>());
    let mut current = head.clone();
    for i in 1..DEPTH {
        let next = ChainNode::new(i);
        current.append(Gc::clone(&next));
        current = next;
    }
    let mut count = 0;
    let mut node_opt = Some(head.clone());
    while let Some(node) = node_opt {
        count += 1;
        let next_ref = node.next.borrow();
        node_opt = next_ref.as_ref().map(Gc::clone);
    }
    assert_eq!(count, DEPTH);
    for _ in 0..5 {
        collect();
    }
    count = 0;
    node_opt = Some(head.clone());
    while let Some(node) = node_opt {
        count += 1;
        let next_ref = node.next.borrow();
        node_opt = next_ref.as_ref().map(Gc::clone);
    }
    assert_eq!(count, DEPTH);
    clear_test_roots();
    drop(head);
    collect();
}

#[derive(Trace)]
struct WideNode {
    id: u64,
    children: GcCell<Vec<Gc<ChildNode>>>,
}

#[derive(Trace)]
struct ChildNode {
    id: u64,
    value: i32,
}

impl WideNode {
    fn new(id: u64) -> Gc<Self> {
        Gc::new(Self {
            id,
            children: GcCell::new(Vec::new()),
        })
    }
    fn add_child(&self, child: Gc<ChildNode>) {
        self.children.borrow_mut().push(child);
    }
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn test_wide_node_1000_children() {
    const CHILDREN: usize = 1_000;
    let parent = WideNode::new(0);
    for i in 0..CHILDREN {
        let child = Gc::new(ChildNode {
            id: i as u64,
            value: i as i32,
        });
        parent.add_child(child);
    }
    assert_eq!(parent.children.borrow().len(), CHILDREN);
    for (i, child) in parent.children.borrow().iter().enumerate() {
        assert_eq!(child.id, i as u64);
        assert_eq!(child.value, i as i32);
    }
    drop(parent);
    collect();
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn test_wide_node_10000_children() {
    const CHILDREN: usize = 10_000;
    let parent = WideNode::new(0);
    for i in 0..CHILDREN {
        let child = Gc::new(ChildNode {
            id: i as u64,
            value: i as i32,
        });
        parent.add_child(child);
    }
    assert_eq!(parent.children.borrow().len(), CHILDREN);
    drop(parent);
    collect();
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn test_wide_node_with_gc_in_between() {
    const CHILDREN_PER_BATCH: usize = 100;
    const BATCHES: usize = 50;
    let parent = WideNode::new(0);
    for batch in 0..BATCHES {
        for i in 0..CHILDREN_PER_BATCH {
            let child = Gc::new(ChildNode {
                id: (batch * CHILDREN_PER_BATCH + i) as u64,
                value: (batch * CHILDREN_PER_BATCH + i) as i32,
            });
            parent.add_child(child);
        }
        collect();
    }
    assert_eq!(parent.children.borrow().len(), CHILDREN_PER_BATCH * BATCHES);
    drop(parent);
    collect();
}

#[derive(Trace)]
struct TrackedNode {
    id: u64,
    next: GcCell<Option<Gc<Self>>>,
}

impl TrackedNode {
    fn new(id: u64) -> Gc<Self> {
        ALLOCATION_COUNT.fetch_add(1, Ordering::SeqCst);
        Gc::new(Self {
            id,
            next: GcCell::new(None),
        })
    }
}

impl Drop for TrackedNode {
    fn drop(&mut self) {
        ALLOCATION_COUNT.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn test_many_allocations_chain() {
    const COUNT: usize = 5_000;
    ALLOCATION_COUNT.store(0, Ordering::SeqCst);
    let mut nodes: Vec<Gc<TrackedNode>> = Vec::new();
    for i in 0..COUNT {
        nodes.push(TrackedNode::new(i as u64));
    }
    for i in 0..COUNT - 1 {
        nodes[i].next.borrow_mut().replace(Gc::clone(&nodes[i + 1]));
    }
    assert_eq!(ALLOCATION_COUNT.load(Ordering::SeqCst), COUNT);
    drop(nodes);
    collect();
    assert_eq!(ALLOCATION_COUNT.load(Ordering::SeqCst), 0);
}

#[test]
fn test_allocation_deallocation_cycle() {
    const CYCLES: usize = 20;
    const NODES_PER_CYCLE: usize = 100;
    ALLOCATION_COUNT.store(0, Ordering::SeqCst);
    for _ in 0..CYCLES {
        let mut nodes: Vec<Gc<TrackedNode>> = Vec::new();
        for i in 0..NODES_PER_CYCLE {
            let node = TrackedNode::new(i as u64);
            nodes.push(node);
        }
        for i in 0..NODES_PER_CYCLE - 1 {
            let next = Gc::clone(&nodes[i + 1]);
            nodes[i].next.borrow_mut().replace(next);
        }
        drop(nodes);
        collect();
        assert_eq!(ALLOCATION_COUNT.load(Ordering::SeqCst), 0);
    }
}

#[test]
#[allow(clippy::cast_sign_loss)]
fn test_nested_scope_allocations() {
    ALLOCATION_COUNT.store(0, Ordering::SeqCst);
    for _ in 0..10 {
        {
            let head = TrackedNode::new(0);
            let mut current = head.clone();
            for i in 1..100 {
                let next = TrackedNode::new(i as u64);
                current.next.borrow_mut().replace(Gc::clone(&next));
                current = next;
            }
        }
        collect();
        assert_eq!(ALLOCATION_COUNT.load(Ordering::SeqCst), 0);
    }
}

#[derive(Trace)]
struct MixedNode {
    id: u64,
    value: i32,
    children: GcCell<Vec<Gc<Self>>>,
    next: GcCell<Option<Gc<Self>>>,
    parent: GcCell<Option<Gc<Self>>>,
}

impl MixedNode {
    fn new(id: u64) -> Gc<Self> {
        Gc::new(Self {
            id,
            value: 0,
            children: GcCell::new(Vec::new()),
            next: GcCell::new(None),
            parent: GcCell::new(None),
        })
    }
}

#[test]
fn test_mixed_structure_dag() {
    const LAYERS: usize = 10;
    const WIDTH: usize = 50;
    let root = MixedNode::new(0);
    let mut layers: Vec<Vec<Gc<MixedNode>>> = Vec::new();
    layers.push(vec![Gc::clone(&root)]);
    for layer in 1..LAYERS {
        let mut nodes: Vec<Gc<MixedNode>> = Vec::new();
        for i in 0..WIDTH {
            let node = MixedNode::new((layer * WIDTH + i) as u64);
            node.parent
                .borrow_mut()
                .replace(Gc::clone(&layers[layer - 1][i % layers[layer - 1].len()]));
            nodes.push(node);
        }
        layers.push(nodes);
    }
    for layer in &layers {
        for (i, node) in layer.iter().enumerate() {
            if i > 0 {
                node.next.borrow_mut().replace(Gc::clone(&layer[i - 1]));
            }
            if i < layer.len() - 1 {
                let sibling = Gc::clone(&layer[i + 1]);
                node.children.borrow_mut().push(sibling);
            }
        }
    }
    drop(root);
    collect();
    for (layer_idx, layer) in layers.iter().enumerate() {
        for node in layer {
            assert!(
                !Gc::is_dead(node),
                "Layer {} node {} should be alive",
                layer_idx,
                node.id
            );
        }
    }
    drop(layers);
    collect();
}

#[test]
fn test_fan_in_pattern() {
    const CHILDREN: usize = 500;
    let parent = MixedNode::new(0);
    for i in 0..CHILDREN {
        let child = MixedNode::new((i + 1) as u64);
        child.parent.borrow_mut().replace(Gc::clone(&parent));
        parent.children.borrow_mut().push(child);
    }
    assert_eq!(parent.children.borrow().len(), CHILDREN);
    for _ in 0..10 {
        let len = parent.children.borrow().len();
        if len > 0 {
            parent.children.borrow_mut().remove(0);
        }
        collect();
    }
    assert!(parent.children.borrow().len() < CHILDREN);
    drop(parent);
    collect();
}

#[test]
fn test_fan_out_pattern() {
    const CHILDREN: usize = 1_000;
    let parent = MixedNode::new(0);
    for i in 0..CHILDREN {
        let child = MixedNode::new((i + 1) as u64);
        parent.children.borrow_mut().push(child);
    }
    assert_eq!(parent.children.borrow().len(), CHILDREN);
    drop(parent);
    collect();
}

#[test]
fn test_rapid_allocation_deallocation() {
    ALLOCATION_COUNT.store(0, Ordering::SeqCst);
    for _ in 0..5 {
        #[allow(clippy::redundant_closure)]
        let nodes: Vec<Gc<TrackedNode>> = (0..1000).map(|i| TrackedNode::new(i)).collect();
        for i in 0..nodes.len() - 1 {
            nodes[i].next.borrow_mut().replace(Gc::clone(&nodes[i + 1]));
        }
        drop(nodes);
        collect();
        assert_eq!(ALLOCATION_COUNT.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn test_incremental_gc_stress() {
    const TOTAL: usize = 5_000;
    const BATCH_SIZE: usize = 100;
    ALLOCATION_COUNT.store(0, Ordering::SeqCst);
    for batch in 0..(TOTAL / BATCH_SIZE) {
        for i in 0..BATCH_SIZE {
            let id = batch * BATCH_SIZE + i;
            let _node = TrackedNode::new(id as u64);
        }
        collect();
    }
    assert_eq!(ALLOCATION_COUNT.load(Ordering::SeqCst), 0);
}
