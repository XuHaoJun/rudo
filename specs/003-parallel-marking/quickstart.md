# Quick Start: Parallel Marking for rudo-gc

**Feature**: Parallel Marking  
**Date**: 2026-01-27

---

## Overview

Parallel marking enables rudo-gc to use multiple CPU cores during the mark phase of garbage collection, significantly reducing GC pause times for multi-threaded applications.

## Adding rudo-gc to Your Project

```toml
# Cargo.toml
[dependencies]
rudo-gc = "0.1"
```

## Basic Usage

Parallel marking is enabled by default. Simply use `Gc<T>` as normal:

```rust
use rudo_gc::{Gc, Trace, collect};

#[derive(Trace)]
struct Node {
    value: i32,
    children: Vec<Gc<Node>>,
}

fn main() {
    // Create a tree structure
    let root = Gc::new(Node {
        value: 0,
        children: Vec::new(),
    });

    // Add many child nodes
    for i in 1..10_000 {
        let child = Gc::new(Node {
            value: i,
            children: Vec::new(),
        });
        root.children.push(child);
    }

    // GC with parallel marking runs automatically
    // or you can manually trigger collection:
    collect();

    println!("Root value: {}", root.value);
}
```

## Multi-Threaded Applications

For multi-threaded applications, parallel marking provides the most benefit:

```rust
use rudo_gc::{Gc, Trace, collect};
use std::thread;

#[derive(Trace)]
struct SharedData {
    id: usize,
    payload: Vec<i32>,
}

fn worker(thread_id: usize, n_objects: usize) -> Vec<Gc<SharedData>> {
    let mut objects = Vec::new();
    for i in 0..n_objects {
        objects.push(Gc::new(SharedData {
            id: thread_id * 1_000_000 + i,
            payload: vec![1, 2, 3, 4, 5],
        }));
    }
    objects
}

fn main() {
    // Spawn multiple threads, each allocating objects
    let handles: Vec<_> = (0..4)
        .map(|i| {
            thread::spawn(move || worker(i, 25_000))
        })
        .collect();

    let all_objects: Vec<_> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();

    println!("Total objects: {}", all_objects.len());

    // Parallel marking will use multiple workers
    collect();

    // Objects remain accessible
    assert_eq!(all_objects.len(), 100_000);
}
```

## Configuration

### Worker Count

By default, rudo-gc uses `min(num_cpus, 16)` workers. You can configure this:

```rust
// Not yet implemented - future API
use rudo_gc::gc::marker::ParallelMarkConfig;

fn configure_gc() {
    let config = ParallelMarkConfig {
        max_workers: 8,  // Use 8 workers
        queue_capacity: 2048,
        parallel_minor_gc: true,
        parallel_major_gc: true,
    };
    rudo_gc::gc::marker::set_config(config);
}
```

### Fallback to Single-Threaded

On systems with 1 CPU core, parallel marking automatically falls back to single-threaded mode with no overhead.

## Performance Expectations

| Configuration | Expected Marking Time |
|---------------|----------------------|
| 1 worker | 1.00x (baseline) |
| 4 workers | 0.35-0.45x |
| 8 workers | 0.25-0.35x |

**Note**: Actual performance depends on workload characteristics, heap size, and object graph structure.

## Best Practices

### 1. Prefer Larger Object Graphs

Parallel marking provides the most benefit when marking large object graphs (10,000+ objects).

### 2. Keep Object Graphs Connected

Objects with many cross-references allow parallel marking to discover more work in parallel.

### 3. Avoid Excessive Fragmentation

While parallel marking helps with pause times, reducing allocation pressure still improves overall performance.

### 4. Use Appropriate Collection Triggers

Let rudo-gc's automatic collection trigger work, or use `collect()` strategically:

```rust
// Good: Collect during known idle periods
fn process_batch(items: &[Item]) {
    for item in items {
        // Process item
    }
    // Collect after batch processing
    collect();
}

// Avoid: Collecting on every iteration
fn bad_example(items: &[Item]) {
    for item in items {
        // Process item
        collect();  // Too frequent!
    }
}
```

## Testing Parallel Marking

### Verify Correctness

```rust
#[cfg(test)]
mod tests {
    use rudo_gc::{Gc, Trace, collect};

    #[derive(Trace)]
    struct Node {
        value: i32,
        next: Option<Gc<Node>>,
    }

    #[test]
    fn test_parallel_marking_completeness() {
        // Create a large linked list
        let mut head = Gc::new(Node {
            value: 0,
            next: None,
        });

        for i in 1..1000 {
            let new_node = Gc::new(Node {
                value: i,
                next: Some(head),
            });
            head = new_node;
        }

        // Collect
        collect();

        // Verify all objects are still accessible
        let mut current = Some(head);
        let mut count = 0;
        while let Some(node) = current {
            count += 1;
            current = node.next.clone();
        }
        assert_eq!(count, 1000);
    }
}
```

### Benchmark Performance

```rust
#[cfg(test)]
mod benchmarks {
    use rudo_gc::{Gc, Trace, collect};
    use std::time::Instant;

    #[derive(Trace)]
    struct Node {
        value: i32,
        children: Vec<Gc<Node>>,
    }

    #[test]
    fn benchmark_parallel_marking() {
        let start = Instant::now();

        // Create 100,000 objects
        let mut nodes: Vec<Gc<Node>> = Vec::new();
        for i in 0..100_000 {
            nodes.push(Gc::new(Node {
                value: i,
                children: Vec::new(),
            }));
        }

        let creation_time = start.elapsed();

        // Measure GC time
        let gc_start = Instant::now();
        collect();
        let gc_time = gc_start.elapsed();

        println!("Creation: {:?}", creation_time);
        println!("GC: {:?}", gc_time);

        // Verify all objects are accessible
        assert_eq!(nodes.len(), 100_000);
    }
}
```

## Troubleshooting

### GC Pauses Still Long

**Possible causes**:
1. Heap is too large (parallel marking helps but doesn't eliminate pauses)
2. Sweep phase is the bottleneck (not yet parallelized)
3. Worker count is limited (check `num_cpus`)

**Solutions**:
- Reduce allocation pressure
- Consider more frequent collections
- Verify worker count is as expected

### Poor Parallel Speedup

**Possible causes**:
1. Object graph is too simple (few cross-references)
2. Work distribution is uneven
3. Contention in mark bitmap updates

**Solutions**:
- Ensure object graph has good connectivity
- Use larger queue capacities
- Profile to identify bottlenecks

### Compilation Errors

Ensure you're using a recent Rust version (1.75+):

```bash
rustc --version  # Should be 1.75.0 or later
```

## API Reference

### Key Types

| Type | Description |
|------|-------------|
| `StealQueue<T, N>` | Lock-free work-stealing queue |
| `PerThreadMarkQueue` | Per-thread work queue |
| `ParallelMarkCoordinator` | Orchestrates parallel marking |
| `ParallelMarkConfig` | Configuration for parallel marking |

### Key Functions

| Function | Description |
|----------|-------------|
| `collect()` | Trigger garbage collection |
| `collect_full()` | Trigger full collection |
| `set_parallel_config()` | Configure parallel marking |

---

## Next Steps

1. **Read the specification**: [spec.md](spec.md)
2. **Understand the data model**: [data-model.md](data-model.md)
3. **Review API contracts**: [contracts/api.md](contracts/api.md)
4. **See implementation details**: [research.md](research.md)
