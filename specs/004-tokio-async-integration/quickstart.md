# Tokio Async/Await Quickstart

**Feature**: 004-tokio-async-integration  
**Date**: 2026-01-30

## Overview

This guide shows how to use rudo-gc with tokio async/await. The integration provides:

- **Manual root guards**: `Gc::root_guard()` for explicit control
- **Proc-macro automation**: `#[gc::main]` and `#[gc::root]`
- **Spawn wrapper**: `gc::spawn()` for automatic root tracking
- **Cooperative scheduling**: `Gc::yield_now()` for GC cooperation

## Enabling the Feature

Add to your `Cargo.toml`:

```toml
[dependencies]
rudo-gc = { version = "0.1", features = ["derive", "tokio"] }
```

## Basic Usage: Manual Root Guards

### Creating and Using Gc in Async Context

```rust
use rudo_gc::{Gc, Trace, GcTokioExt};

#[derive(Trace)]
struct Data {
    value: i32,
}

async fn example() {
    // Create a Gc pointer
    let gc = Gc::new(Data { value: 42 });

    // Manually create a root guard
    let _guard = gc.root_guard();

    // Spawn a task that accesses the Gc
    tokio::spawn(async move {
        // gc is valid because guard exists
        println!("Value: {}", gc.value);
    }).await.unwrap();

    // Guard dropped here, gc no longer protected
}
```

### How It Works

1. `gc.root_guard()` creates a `GcRootGuard` that registers the Gc pointer
2. The guard keeps the Gc alive while it exists
3. When the guard is dropped, the pointer is unregistered
4. The GC will not collect the Gc while any guard exists

## Proc-Macro Automation

### #[gc::main]

Automatically initializes GcRootSet and creates a tokio runtime:

```rust
use rudo_gc::{Gc, Trace};

#[gc::main]
async fn main() {
    let gc = Gc::new(Data { value: 42 });

    tokio::spawn(async move {
        println!("Value: {}", gc.value);
    }).await.unwrap();
}
```

**Options** (similar to `#[tokio::main]`):

```rust
#[gc::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    // Multi-threaded runtime with 4 workers
}
```

### #[gc::root]

Automatically wraps an async block with a root guard:

```rust
use rudo_gc::{Gc, Trace};

#[gc::main]
async fn main() {
    let gc = Gc::new(Data { value: 42 });

    // gc::root automatically creates a guard for the block
    gc::spawn(async {
        println!("Value: {}", gc.value);
        // Guard active for this block's lifetime
    }).await.unwrap();
}
```

## Spawn Wrapper

### gc::spawn()

Automatically tracks Gc roots when spawning tasks:

```rust
use rudo_gc::tokio::spawn;

#[gc::main]
async fn main() {
    let gc = Gc::new(Data { value: 42 });

    // Root is automatically protected for the task's lifetime
    spawn(async move {
        println!("Value: {}", gc.value);
    }).await.unwrap();
}
```

**Equivalent to**:

```rust
let _guard = gc.root_guard();
tokio::spawn(async move {
    // Guard protects gc
}).await.unwrap()
```

## Cooperative Scheduling

### Gc::yield_now()

Yields to the tokio scheduler, allowing the GC to run:

```rust
use rudo_gc::GcTokioExt;

async fn long_computation() {
    let gc = Gc::new(large_data());

    for i in 0..10000 {
        // Process data
        do_work(&gc, i);

        // Periodically yield to allow GC
        gc.yield_now().await;
    }
}
```

## Complete Example

```rust
use rudo_gc::{Gc, Trace, GcTokioExt};
use rudo_gc::tokio::spawn;

#[derive(Trace)]
struct Counter {
    value: i32,
}

#[gc::main]
async fn main() {
    let counter = Gc::new(Counter { value: 0 });

    // Spawn multiple tasks with automatic root tracking
    let handles: Vec<_> = (0..10).map(|i| {
        let counter = counter.clone();
        spawn(async move {
            for j in 0..100 {
                // counter.value is accessible because gc::spawn
                // automatically protects it
                counter.value += 1;
                counter.yield_now().await;
            }
            counter.value
        })
    }).collect();

    // Wait for all tasks and sum results
    let total: i32 = handles
        .into_iter()
        .map(|h| h.await.unwrap())
        .sum();

    println!("Total: {}", total); // Total: 1000
}
```

## Testing

### Integration Test Pattern

```rust
#[cfg(test)]
mod tests {
    use rudo_gc::{Gc, Trace, GcTokioExt};
    use rudo_gc::tokio::spawn;

    #[derive(Trace)]
    struct TestData {
        value: i32,
    }

    #[tokio::test]
    async fn test_gc_survives_task() {
        let gc = Gc::new(TestData { value: 42 });

        // Guard protects gc during task
        let _guard = gc.root_guard();

        spawn(async {
            // gc must still be valid here
            assert_eq!(gc.value, 42);
        }).await.unwrap();
    }

    #[tokio::test]
    async fn test_gc_collected_after_guard_drop() {
        let gc = Gc::new(TestData { value: 42);
        let ptr = Gc::into_raw(gc) as usize;

        {
            let gc = unsafe { Gc::from_raw(ptr as *mut TestData) };
            let _guard = gc.root_guard();

            // gc is protected
        }

        // After guard drops, gc can be collected
        // Run GC to verify
        rudo_gc::force_collect();

        // Verify object was collected (implementation-specific)
    }
}
```

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Guard dropped too early | Gc becomes eligible for collection |
| No guard when accessing Gc in task | Undefined behavior (use gc::spawn) |
| yield_now outside tokio context | Panics (must be in tokio runtime) |

## Performance Tips

1. **Use gc::spawn()**: Automatic tracking is more efficient than manual guards
2. **Yield periodically**: Long-running tasks should call `yield_now()` to allow GC
3. **Batch operations**: Group Gc operations to minimize guard lifetime
4. **Avoid nested guards**: Multiple guards for same pointer are redundant
