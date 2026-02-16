//! GUI Overall Benchmark - Fine-grained Reactivity (Solid.js/Svelte style)
//!
//! Simulates UI Tree + Reactive state patterns for native GUI frameworks.
//! Based on Solid.js/Svelte fine-grained reactivity (not VDOM).

use criterion::{criterion_group, criterion_main, Criterion};
use rudo_gc::handles::HandleScope;
use rudo_gc::heap::with_heap_and_tcb;
use rudo_gc::{collect_full, Gc, GcCell, Trace};
use std::cell::Cell;
use std::hint::black_box;

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

    fn add_signal(&self, signal: Gc<Signal>) {
        self.signals.borrow_mut().push(signal);
    }

    fn add_child(&self, child: Gc<Self>) {
        self.children.borrow_mut().push(child);
    }
}

struct BenchmarkState {
    root: Option<Gc<UiNode>>,
    node_count: usize,
    signal_count: usize,
}

impl BenchmarkState {
    const fn new() -> Self {
        Self {
            root: None,
            node_count: 0,
            signal_count: 0,
        }
    }

    #[allow(dead_code)]
    fn clear(&mut self) {
        self.root = None;
        self.node_count = 0;
        self.signal_count = 0;
    }

    fn alloc_signal(&mut self) -> Gc<Signal> {
        self.signal_count += 1;
        Gc::new(Signal {
            value: Cell::new(0),
            observers: Vec::new(),
        })
    }

    fn build_tree_recursive(&mut self, parent: &Gc<UiNode>, depth: usize) {
        if depth > TREE_MAX_DEPTH {
            return;
        }

        let branches = if depth < TREE_MIN_DEPTH {
            MAX_BRANCHES
        } else {
            MIN_BRANCHES + (self.node_count % (MAX_BRANCHES - MIN_BRANCHES + 1))
        };

        for _ in 0..branches {
            if self.node_count >= 10000 {
                break;
            }

            let node = UiNode::new(self.node_count);
            self.node_count += 1;

            // Add signals to node
            for _ in 0..SIGNALS_PER_NODE {
                let signal = self.alloc_signal();
                node.add_signal(signal);
            }

            // Link parent-child
            *parent.parent.borrow_mut() = Some(Gc::clone(parent));
            parent.add_child(node.clone());

            // Recurse
            self.build_tree_recursive(&node, depth + 1);
        }
    }

    fn build_tree(&mut self) {
        let root = UiNode::new(0);
        self.node_count = 1;

        // Add signals to root
        for _ in 0..SIGNALS_PER_NODE {
            let signal = self.alloc_signal();
            root.add_signal(signal);
        }

        self.root = Some(root.clone());
        self.build_tree_recursive(&root, 1);
    }

    fn reactive_update(&self, count: usize, scope: &HandleScope<'_>) {
        // Simple update: iterate through signals and update values
        // This simulates reactive updates in fine-grained reactivity
        // Use HandleScope for explicit rooting during updates
        for _ in 0..count {
            if let Some(ref root) = self.root {
                let handle = scope.handle(root);
                let mut signals = handle.signals.borrow_mut();
                for signal in signals.iter_mut().take(1) {
                    signal.value.set(signal.value.get() + 1);
                }
            }
        }
    }

    fn destroy_half(&mut self, scope: &HandleScope<'_>) {
        // Set children to empty to simulate partial destroy
        // Use HandleScope for explicit rooting during mutation
        if let Some(ref root) = self.root {
            let handle = scope.handle(root);
            let mut children = handle.children.borrow_mut();
            let half = children.len() / 2;
            children.truncate(half);
        }
        self.node_count /= 2;
    }
}

fn bench_tree_build_1k(c: &mut Criterion) {
    c.bench_function("tree_build_1k", |b| {
        b.iter(|| {
            with_heap_and_tcb(|_, tcb| {
                let scope = HandleScope::new(tcb);
                let mut state = BenchmarkState::new();
                state.build_tree();
                black_box(&scope);
                black_box(&state);
            });
        });
    });
}

fn bench_tree_build_10k(c: &mut Criterion) {
    c.bench_function("tree_build_10k", |b| {
        b.iter(|| {
            with_heap_and_tcb(|_, tcb| {
                let scope = HandleScope::new(tcb);
                let mut state = BenchmarkState::new();
                state.build_tree();
                black_box(&scope);
                black_box(&state);
            });
        });
    });
}

fn bench_reactive_update_1k(c: &mut Criterion) {
    let mut state = BenchmarkState::new();
    with_heap_and_tcb(|_, tcb| {
        state.build_tree();

        c.bench_function("reactive_update_1k", |b| {
            b.iter(|| {
                let scope = HandleScope::new(tcb);
                state.reactive_update(1000, &scope);
                black_box(());
            });
        });
    });
}

fn bench_reactive_update_10k(c: &mut Criterion) {
    let mut state = BenchmarkState::new();
    with_heap_and_tcb(|_, tcb| {
        state.build_tree();

        c.bench_function("reactive_update_10k", |b| {
            b.iter(|| {
                let scope = HandleScope::new(tcb);
                state.reactive_update(10000, &scope);
                black_box(());
            });
        });
    });
}

fn bench_partial_destroy(c: &mut Criterion) {
    let mut state = BenchmarkState::new();
    with_heap_and_tcb(|_, tcb| {
        state.build_tree();

        c.bench_function("partial_destroy", |b| {
            b.iter(|| {
                let scope = HandleScope::new(tcb);
                state.destroy_half(&scope);
                black_box(&state);
                collect_full();
            });
        });
    });
}

fn bench_full_cycle(c: &mut Criterion) {
    c.bench_function("full_cycle", |b| {
        b.iter(|| {
            with_heap_and_tcb(|_, tcb| {
                let scope = HandleScope::new(tcb);

                // Build
                let mut state = BenchmarkState::new();
                state.build_tree();
                black_box(&state);

                // Update
                state.reactive_update(1000, &scope);

                // Destroy
                state.destroy_half(&scope);
                collect_full();

                black_box(());
            });
        });
    });
}

fn bench_sustained_60fps(c: &mut Criterion) {
    let mut state = BenchmarkState::new();
    with_heap_and_tcb(|_, tcb| {
        state.build_tree();

        let updates_per_frame = 100;

        c.bench_function("sustained_60fps", |b| {
            b.iter(|| {
                let scope = HandleScope::new(tcb);
                for _ in 0..60 {
                    state.reactive_update(updates_per_frame, &scope);
                    black_box(());
                }
            });
        });
    });
}

criterion_group!(
    name = gui_overall;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_millis(100))
        .measurement_time(std::time::Duration::from_millis(500))
        .noise_threshold(0.10)
        .confidence_level(0.90);
    targets =
        bench_tree_build_1k,
        bench_tree_build_10k,
        bench_reactive_update_1k,
        bench_reactive_update_10k,
        bench_partial_destroy,
        bench_full_cycle,
        bench_sustained_60fps,
);
criterion_main!(gui_overall);
