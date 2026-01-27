//! Integration tests for parallel garbage collection.
//!
//! These tests verify the correctness and performance of parallel marking
//! across multiple worker threads.

use std::sync::Arc;
use std::thread;

use rudo_gc::{collect, Gc, Trace};

#[derive(Trace)]
struct Node {
    value: usize,
    children: Vec<Gc<Self>>,
}

#[derive(Trace)]
struct LinkedNode {
    value: usize,
    next: Option<Gc<Self>>,
}

#[derive(Trace)]
struct TreeNode {
    value: usize,
    left: Option<Gc<Self>>,
    right: Option<Gc<Self>>,
}

#[derive(Trace)]
struct SimpleNode {
    value: usize,
}

#[derive(Trace)]
struct CrossThreadNode {
    value: usize,
    next: Option<Gc<Self>>,
}

#[derive(Trace)]
struct LargeNode {
    value: usize,
    children: Vec<Gc<Self>>,
}

#[test]
fn test_parallel_major_gc() {
    let mut nodes: Vec<Gc<LinkedNode>> = Vec::new();

    for i in 0..1000 {
        nodes.push(Gc::new(LinkedNode {
            value: i,
            next: if i > 0 {
                Some(Gc::clone(&nodes[i - 1]))
            } else {
                None
            },
        }));
    }

    collect();

    let mut current = Some(Gc::clone(&nodes[999]));
    let mut count = 0;
    while let Some(node) = current {
        count += 1;
        current = node.next.clone();
    }

    assert_eq!(count, 1000);
}

#[test]
fn test_parallel_minor_gc() {
    #[derive(Trace)]
    struct Young {
        value: usize,
    }

    let mut young_objects: Vec<Gc<Young>> = Vec::new();

    for i in 0..500 {
        young_objects.push(Gc::new(Young { value: i }));
    }

    collect();

    assert_eq!(young_objects.len(), 500);
}

#[test]
fn test_parallel_gc_with_complex_graph() {
    let mut nodes: Vec<Gc<TreeNode>> = Vec::new();

    for i in 0..100 {
        nodes.push(Gc::new(TreeNode {
            value: i,
            left: if i > 0 {
                Some(Gc::clone(&nodes[i - 1]))
            } else {
                None
            },
            right: if i % 3 == 0 && i > 0 {
                Some(Gc::clone(&nodes[i / 3]))
            } else {
                None
            },
        }));
    }

    collect();

    assert_eq!(nodes.len(), 100);
}

#[test]
fn test_multi_thread_local_heap_dirty_pages() {
    let mut nodes: Vec<Gc<SimpleNode>> = Vec::new();

    for i in 0..100 {
        nodes.push(Gc::new(SimpleNode { value: i }));
    }

    collect();

    assert_eq!(nodes.len(), 100);
}

#[test]
fn test_work_stealing_balance() {
    let mut nodes: Vec<Gc<LargeNode>> = Vec::new();

    for i in 0..1000 {
        let mut children = Vec::new();
        for j in 0..10 {
            children.push(Gc::new(LargeNode {
                value: i * 10 + j,
                children: Vec::new(),
            }));
        }
        nodes.push(Gc::new(LargeNode { value: i, children }));
    }

    collect();

    assert_eq!(nodes.len(), 1000);
}

#[test]
fn test_steal_from_other_queues() {
    let node = Gc::new(SimpleNode { value: 42 });

    collect();

    assert_eq!(node.value, 42);
}

#[test]
fn test_cross_thread_references() {
    let node1 = Arc::new(Gc::new(CrossThreadNode {
        value: 1,
        next: None,
    }));
    let node2 = Arc::new(Gc::new(CrossThreadNode {
        value: 2,
        next: Some(Gc::clone(&node1)),
    }));

    let handle = thread::spawn(move || {
        Gc::new(CrossThreadNode {
            value: 3,
            next: Some(Gc::clone(&node2)),
        })
    });

    let node3 = handle.join().unwrap();

    collect();

    assert_eq!(node3.value, 3);
    assert_eq!(node3.next.as_ref().unwrap().value, 2);
    assert_eq!(node3.next.as_ref().unwrap().next.as_ref().unwrap().value, 1);
}

#[test]
fn test_three_thread_object_chain() {
    let node1 = Arc::new(Gc::new(CrossThreadNode {
        value: 1,
        next: None,
    }));

    let node1_clone = Arc::clone(&node1);
    let handle1 = thread::spawn(move || {
        Gc::new(CrossThreadNode {
            value: 2,
            next: Some(Gc::clone(&node1_clone)),
        })
    });

    let node2 = handle1.join().unwrap();
    let node2_clone = Arc::new(node2);

    let handle2 = thread::spawn(move || {
        Gc::new(CrossThreadNode {
            value: 3,
            next: Some(Gc::clone(&node2_clone)),
        })
    });

    let node3 = handle2.join().unwrap();

    collect();

    assert_eq!(node3.value, 3);
    assert_eq!(node3.next.as_ref().unwrap().value, 2);
    assert_eq!(node3.next.as_ref().unwrap().next.as_ref().unwrap().value, 1);
}

#[test]
fn test_no_unnecessary_gc_during_drop() {
    use std::time::Duration;
    use std::time::Instant;

    let node_count = 50_000;

    let mut nodes: Vec<Gc<Node>> = Vec::new();
    for i in 0..node_count {
        nodes.push(Gc::new(Node {
            value: i,
            children: Vec::new(),
        }));
    }

    collect();

    let drop_start = Instant::now();
    drop(nodes);
    let drop_duration = drop_start.elapsed();

    assert!(
        drop_duration < Duration::from_millis(100),
        "Dropping {} objects took {}ms, expected <100ms. \
         This indicates N_EXISTING is being corrupted during GC sweep, \
         causing unnecessary GC cycles.",
        node_count,
        drop_duration.as_millis()
    );
}
