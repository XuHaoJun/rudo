//! GUI Overall Benchmark - Simple version
//!
//! A simple benchmark that runs and exits properly

use rudo_gc::{collect_full, Gc, GcCell, Trace};
use std::cell::Cell;
use std::time::Instant;

const SIGNALS_PER_NODE: usize = 5;
const TREE_MIN_DEPTH: usize = 3;
const TREE_MAX_DEPTH: usize = 8;
const MIN_BRANCHES: usize = 2;
const MAX_BRANCHES: usize = 6;

#[derive(Trace)]
struct Signal {
    value: Cell<usize>,
    observers: Vec<usize>,
}

#[derive(Trace)]
struct UiNode {
    id: usize,
    parent: GcCell<Option<Gc<Self>>>,
    children: GcCell<Vec<Gc<Self>>>,
    signals: GcCell<Vec<Gc<Signal>>>,
}

impl UiNode {
    fn new(id: usize) -> Gc<Self> {
        Gc::new(Self {
            id,
            parent: GcCell::new(None),
            children: GcCell::new(Vec::new()),
            signals: GcCell::new(Vec::new()),
        })
    }

    #[allow(dead_code)]
    fn add_signal(&self, signal: Gc<Signal>) {
        self.signals.borrow_mut().push(signal);
    }

    fn add_child(&self, child: Gc<Self>) {
        self.children.borrow_mut().push(child);
    }
}

fn build_tree() {
    let root = UiNode::new(0);
    let mut node_count = 1;

    // Add signals to root
    for _ in 0..SIGNALS_PER_NODE {
        let _signal = Gc::new(Signal {
            value: Cell::new(0),
            observers: Vec::new(),
        });
    }

    // Build recursively
    #[allow(clippy::items_after_statements)]
    fn build_recursive(parent: &Gc<UiNode>, depth: usize, node_count: &mut usize) {
        if depth > TREE_MAX_DEPTH {
            return;
        }

        let branches = if depth < TREE_MIN_DEPTH {
            MAX_BRANCHES
        } else {
            MIN_BRANCHES + (*node_count % (MAX_BRANCHES - MIN_BRANCHES + 1))
        };

        for _ in 0..branches {
            if *node_count >= 10000 {
                break;
            }

            let node = UiNode::new(*node_count);
            *node_count += 1;

            // Add signals
            for _ in 0..SIGNALS_PER_NODE {
                let _signal = Gc::new(Signal {
                    value: Cell::new(0),
                    observers: Vec::new(),
                });
            }

            // Link parent-child
            *parent.parent.borrow_mut() = Some(Gc::clone(parent));
            parent.add_child(node.clone());

            // Recurse
            build_recursive(&node, depth + 1, node_count);
        }
    }

    build_recursive(&root, 1, &mut node_count);
}

fn main() {
    println!("Running GUI benchmark...");

    // Warmup
    for _ in 0..3 {
        build_tree();
    }

    // Run benchmark
    let start = Instant::now();
    build_tree();
    let duration = start.elapsed();

    println!("Build tree time: {duration:?}");

    // Run GC
    let gc_start = Instant::now();
    collect_full();
    let gc_duration = gc_start.elapsed();

    println!("GC time: {gc_duration:?}");
    println!("Total time: {:?}", duration + gc_duration);
}
