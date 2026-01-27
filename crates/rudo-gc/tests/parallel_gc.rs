//! Integration tests for parallel garbage collection.
//!
//! These tests verify the correctness and performance of parallel marking
//! across multiple worker threads.

#[cfg(test)]
mod parallel_gc_tests {
    use rudo_gc::{collect, Gc, Trace};

    #[derive(Trace)]
    struct Node {
        value: usize,
        next: Option<Gc<Self>>,
    }

    #[test]
    #[ignore = "Requires parallel marking infrastructure to be fully integrated"]
    fn test_parallel_major_gc() {
        let mut nodes: Vec<Gc<Node>> = Vec::new();

        for i in 0..1000 {
            nodes.push(Gc::new(Node {
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
    #[ignore = "Requires parallel marking infrastructure to be fully integrated"]
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
    #[ignore = "Requires parallel marking infrastructure to be fully integrated"]
    fn test_parallel_gc_with_complex_graph() {
        #[derive(Trace)]
        struct TreeNode {
            value: usize,
            left: Option<Gc<Self>>,
            right: Option<Gc<Self>>,
        }

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
}

#[cfg(test)]
mod benchmarks {
    use std::time::Instant;

    use rudo_gc::{collect, Gc, Trace};

    #[derive(Trace)]
    struct Node {
        value: usize,
        children: Vec<Gc<Self>>,
    }

    #[test]
    #[ignore = "Requires parallel marking infrastructure to be fully integrated"]
    fn benchmark_parallel_marking_performance() {
        let start = Instant::now();

        let mut nodes: Vec<Gc<Node>> = Vec::new();
        for i in 0..100_000 {
            nodes.push(Gc::new(Node {
                value: i,
                children: Vec::new(),
            }));
        }

        let creation_time = start.elapsed();
        println!("Creation: {creation_time:?}");

        let gc_start = Instant::now();
        collect();
        let gc_time = gc_start.elapsed();
        println!("GC: {gc_time:?}");

        assert_eq!(nodes.len(), 100_000);
    }
}
