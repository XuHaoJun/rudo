# rudo-gc

[![Crates.io](https://img.shields.io/crates/v/rudo-gc.svg)](https://crates.io/crates/rudo-gc)
[![Documentation](https://docs.rs/rudo-gc/badge.svg)](https://docs.rs/rudo-gc)
[![License](https://img.shields.io/crates/l/rudo-gc.svg)](https://github.com/xuhaojun/rudo/blob/main/LICENSE)

A high-performance, generational, non-moving garbage collector for Rust, inspired by the architecture of **Chez Scheme** and the pragmatism of **V8**.

## Overview

`rudo-gc` provides a `Gc<T>` smart pointer that offers automatic memory reclamation and cycle detection. Unlike `Rc<T>` or `Arc<T>`, it can handle complex cyclic data structures without requiring `Weak` pointers to break cycles manually.

The library is built on a **BiBOP (Big Bag of Pages)** memory layout, which allows for extremely fast O(1) allocation and efficient metadata lookup from any pointer.

## Features

- **Generational Garbage Collection**: Optimized for the "generational hypothesis" (most objects die young) with distinct Minor and Major collection phases.
- **BiBOP Memory Layout**: Objects are grouped by size classes into 4KB pages, enabling O(1) allocation and non-intrusive metadata storage.
- **Address Stability**: A non-moving collector ensures that `&T` references to GC-managed data remain valid during the object's lifetime.
- **Conservative Stack Scanning**: Automatically discovers roots on the stack and in registers, minimizing the need for manual root registration. Now supports **Linux, macOS, and Windows**.
- **Address Space Coloring**: Uses heap capability hints to place memory in safe regions, reducing false positives during conservative stack scanning.
- **Write Barriers**: Efficiently tracks old-to-young pointers using card-marking (dirty bitmaps) for fast minor collections.
- **Incremental Marking**: Splits major GC mark phase into cooperative increments, reducing pause times by 50-80%. Uses hybrid SATB + Dijkstra insertion barrier.
- **Large Object Space (LOS)**: Specialized handling for objects larger than 2KB to prevent fragmentation.
- **Weak References**: Support for `Weak<T>` pointers with proper lifecycle management.
- **ZST Optimization**: Zero-Sized Types (like `()`) are handled with zero heap allocation overhead.
- **Thread Safety**: `Gc<T>` implements `Send` and `Sync` when `T: Send + Sync`, enabling safe multi-threaded data sharing. Use `GcRwLock` and `GcMutex` for concurrent access.
- **Concurrent Primitives**: `GcRwLock<T>` and `GcMutex<T>` provide thread-safe locking with automatic write barriers and GC-safe lock bypass during STW pauses.
- **Cross-Thread Handles**: `GcHandle<T>` and `WeakCrossThreadHandle<T>` allow safe handoff of GC-managed objects between threads without requiring `T: Send + Sync`. Resolution is enforced at runtime on the origin thread only.
- **Parallel Marking**: Work-stealing based parallel marking for multi-core scalability.
- **Lazy Sweep**: Defers memory reclamation to allocation time, reducing STW pause times (enabled by default).
- **HandleScope**: V8-style explicit rooting for maximum performance and compiler-checked safety.
- **Tokio Async Integration**: Full support for async/await with `spawn_with_gc!`, `AsyncHandleScope`, and `#[gc::main]` macro.
- **GcCell Derive Macro**: `#[derive(GcCell)]` automatically implements `GcCapture` for types with `Gc<T>` fields, simplifying SATB barrier usage.
- **Tracing Support**: Optional structured logging of GC operations using the `tracing` crate for observability and debugging.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
rudo-gc = "0.7"
```

If you want to use the `#[derive(Trace)]` macro, enable the `derive` feature:

```toml
[dependencies]
rudo-gc = { version = "0.7", features = ["derive"] }
```

For Tokio async/await integration:

```toml
[dependencies]
rudo-gc = { version = "0.7", features = ["derive", "tokio"] }
```

### Tracing Support (Opt-in)

Enable the `tracing` feature to get structured logging of GC operations using the `tracing` crate:

```toml
[dependencies]
rudo-gc = { version = "0.7", features = ["tracing"] }
```

Then configure a tracing subscriber in your application:

```rust
use tracing_subscriber::{fmt, EnvFilter};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("rudo_gc=debug"))
        .init();

    // Your GC-using code here
    let gc = Gc::new(42);
    collect(); // GC events will appear in logs
}
```

When enabled, you'll see structured spans for each garbage collection:

```
DEBUG rudo_gc::gc: gc_collect collection_type="major_multi_threaded" gc_id=42
DEBUG rudo_gc::gc: gc_phase phase="clear"
DEBUG rudo_gc::gc: phase_start phase="clear" bytes_before=10485760
DEBUG rudo_gc::gc: phase_end phase="clear" bytes_reclaimed=0
```

### Incremental Marking (Opt-in)

Incremental marking is available starting from v0.7. Enable it to reduce major GC pause times:

```toml
[dependencies]
rudo-gc = { version = "0.7", features = ["incremental"] }
```

Then configure it in your code:

```rust
use rudo_gc::IncrementalConfig;

let config = IncrementalConfig {
    enabled: true,
    increment_size: 1000,
    ..Default::default()
};
rudo_gc::set_incremental_config(config);
```

### Lazy Sweep (Enabled by Default)

The `lazy-sweep` feature (enabled by default) defers memory reclamation to allocation time, reducing STW pause times. To disable:

```toml
[dependencies]
rudo-gc = { version = "0.7", default-features = false }
```

Lazy sweep is recommended for applications where latency matters more than peak throughput. The eager sweep path (when disabled) may perform better in batch processing workloads.

## Migration from v0.6 to v0.7

Version 0.7 introduces a simplified `GcCell` API.

### Key Changes

1. **`GcCell::borrow_mut()` is now the primary method** - It automatically handles all barrier types correctly
2. **Types need `#[derive(GcCell)]`** to work with `GcCell<T>` containing `Gc<T>` fields
3. **`borrow_mut_with_satb()` is deprecated** - Use `borrow_mut()` instead

### Before (v0.6)

```rust
#[derive(Trace)]
struct Node {
    next: GcCell<Option<Gc<Node>>>,  // Error - needs GcCapture!
}

let cell = GcCell::new(Node { ... });
*cell.borrow_mut() = ...;  // Error!
```

### After (v0.7)

```rust
#[derive(Trace, GcCell)]  // Add GcCell derive
struct Node {
    next: GcCell<Option<Gc<Node>>>,
}

let cell = GcCell::new(Node { ... });
*cell.borrow_mut() = ...;  // Works!
```

### Summary of Changes

| Change | v0.6 | v0.7 |
|--------|-------|-------|
| `GcCell<Gc<T>>::borrow_mut()` | Error | ✅ Works |
| Derive required | ❌ | ✅ `#[derive(GcCell)]` |
| Barrier complexity | Multiple methods | Single method `borrow_mut()` |

## Quick Start

```rust
use rudo_gc::{Gc, Trace, cell::GcCell, GcCell};

// Simple allocation
let x = Gc::new(42);
println!("Value: {}", *x);

// Custom types with derive
#[derive(Trace, GcCell)]
struct Node {
    value: i32,
    next: GcCell<Option<Gc<Node>>>,
}

let node = Gc::new(Node {
    value: 1,
    next: GcCell::new(Some(Gc::new(Node {
        value: 2,
        next: GcCell::new(None)
    })))
});

// Mutating a GC-managed object
*node.next.borrow_mut() = None;
```

## GcCell API

`GcCell<T>` provides interior mutability with write barriers for GC-managed objects.

### Simple Usage

```rust
use rudo_gc::{Gc, Trace, cell::GcCell, GcCell};

// Derive GcCell for types that will be used with GcCell
#[derive(Trace, GcCell)]
struct Node {
    value: i32,
    next: GcCell<Option<Gc<Node>>>,
}

// Use borrow_mut() for mutation
let cell = GcCell::new(Node {
    value: 1,
    next: GcCell::new(None),
});
*cell.borrow_mut() = Node {
    value: 100,
    next: GcCell::new(None),
};
```

### Advanced Usage

For performance-critical code, use `borrow_mut_gen_only()`:

```rust
// No barriers - fastest option but may cause incorrect GC
let cell = GcCell::new(expensive_computation());
*cell.borrow_mut_gen_only() = result;
```

**Note**: `borrow_mut_gen_only()` is unsafe if the type contains `Gc<T>` pointers.

## GcCell Derive Macro

The `#[derive(GcCell)]` macro automatically implements `GcCapture` for types containing `Gc<T>` fields, enabling SATB barrier correctness without manual implementation.

### Usage

```rust
use rudo_gc::{Gc, Trace, cell::GcCell, GcCell};

// Derive GcCell for types with Gc<T> fields
#[derive(Trace, GcCell)]
struct Node {
    value: i32,
    next: GcCell<Option<Gc<Node>>>,  // Automatically implements GcCapture
}

// Types without Gc<T> fields get an empty GcCapture impl
#[derive(Trace, GcCell)]
struct SimpleStruct {
    value: i32,
    name: String,
}
```

### What It Generates

For types with `Gc<T>` fields:

```rust
impl GcCapture for Node {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        self.next.capture_gc_ptrs_into(ptrs);
    }
}
```

### Supported Types

- `Gc<T>`
- `Vec<Gc<T>>`
- `Option<Gc<T>>`
- `GcCell<Gc<T>>`
- `GcCell<Vec<Gc<T>>>`
- `GcCell<Option<Gc<T>>>`
- Unnamed structs (tuple-like)
- Nested types (types that implement `GcCapture`)

### Limitations

- Enums: Not supported (use manual implementation)
- Generic types: Not supported (use manual implementation)
- Recursive types: Not supported (use manual implementation)

## GcRwLock and GcMutex

`GcRwLock<T>` and `GcMutex<T>` provide thread-safe concurrent access to GC-managed objects. Use these when sharing data between threads.

### When to Use Each

| Type | Use Case |
|------|----------|
| `GcRwLock<T>` | Read-heavy workloads, multiple readers concurrent |
| `GcMutex<T>` | Write-heavy workloads, simple exclusive access |

### GcRwLock Example

```rust
use rudo_gc::{Gc, GcRwLock, Trace};

#[derive(Trace)]
struct SharedData {
    value: i32,
    items: Vec<i32>,
}

let data: Gc<GcRwLock<SharedData>> = Gc::new(GcRwLock::new(SharedData {
    value: 0,
    items: Vec::new(),
}));

// Multiple readers can access concurrently
let readers: Vec<_> = (0..4).map(|_| {
    let data = Gc::clone(&data);
    std::thread::spawn(move || {
        for _ in 0..100 {
            let guard = data.read();
            println!("Reader saw: {}", guard.value);
        }
    })
}).collect();

// Writer has exclusive access
let mut guard = data.write();
guard.value = 42;
guard.items.push(1);
drop(guard);

for handle in readers {
    handle.join().unwrap();
}
```

### GcMutex Example

```rust
use rudo_gc::{Gc, GcMutex, Trace};

#[derive(Trace)]
struct Counter {
    count: i32,
}

let counter: Gc<GcMutex<Counter>> = Gc::new(GcMutex::new(Counter { count: 0 }));

// Multiple threads increment the counter
let handles: Vec<_> = (0..4).map(|_| {
    let counter = Gc::clone(&counter);
    std::thread::spawn(move || {
        for _ in 0..100 {
            let mut guard = counter.lock();
            guard.count += 1;
        }
    })
}).collect();

for handle in handles {
    handle.join().unwrap();
}

assert_eq!(counter.lock().count, 400);
```

### Write Barriers

Both `GcRwLock::write()` and `GcMutex::lock()` automatically trigger generational and SATB write barriers on guard acquisition. This ensures correct GC tracking during incremental marking.

### Comparison with GcCell

| Characteristic | GcCell | GcRwLock | GcMutex |
|----------------|--------|----------|---------|
| Threading | !Sync | Sync + Send | Sync + Send |
| Multiple readers | No | Yes | No |
| Write barriers | On borrow_mut() | On write() | On lock() |
| Use case | Single-threaded DOM, AST | Caches, configs | Queues, state machines |

## Cross-Thread GC Handles

`GcHandle<T>` and `WeakCrossThreadHandle<T>` enable safe transfer of GC-managed objects between threads without requiring `T: Send + Sync`. Handles can be sent through channels or stored in other threads, but can only be resolved back to `Gc<T>` on the thread where they were created.

### When to Use Cross-Thread Handles

| Scenario | Recommended API |
|----------|-----------------|
| Non-Send signal types shared across threads | `GcHandle<T>` |
| Checking if object is still alive without preventing collection | `WeakCrossThreadHandle<T>` |
| Thread-uncertain contexts | `try_resolve()` for graceful fallback |

### Creating Handles

```rust
use rudo_gc::{Gc, Trace, GcHandle};

#[derive(Trace)]
struct SignalData {
    value: i32,
}

let gc: Gc<SignalData> = Gc::new(SignalData { value: 42 });
let handle: GcHandle<SignalData> = gc.cross_thread_handle();
```

### Sending Between Threads

Handles implement `Send + Sync` regardless of the inner type:

```rust
use std::sync::mpsc;
use std::thread;

let (sender, receiver) = mpsc::channel::<GcHandle<SignalData>>();

thread::spawn(move || {
    sender.send(handle).unwrap();
});

let received_handle = receiver.recv().unwrap();
```

### Resolving Handles

Resolution is enforced at runtime on the origin thread:

```rust
// Works on the origin thread
let signal: Gc<SignalData> = handle.resolve();
```

For graceful handling when thread identity is uncertain:

```rust
if let Some(signal) = handle.try_resolve() {
    signal.value += 1;
} else {
    // Wrong thread - queue for later
    other_thread_sender.send(handle.clone());
}
```

### Weak Handles

```rust
let weak: WeakCrossThreadHandle<SignalData> = gc.weak_cross_thread_handle();

if weak.is_valid() {
    let signal = weak.upgrade().unwrap();
    // Use signal...
}
```

### Safety Guarantees

1. **Thread-Safe Handles**: `GcHandle<T>` implements `Send + Sync` even when `T` does not
2. **Origin Thread Enforcement**: Resolution panics if attempted from wrong thread
3. **Liveness Guarantee**: Strong handles prevent collection while registered
4. **Safe Drop**: Handles can be dropped from any thread
5. **Graceful Degradation**: `try_resolve()` returns `None` instead of panicking

### HandleScope - Optional

`rudo-gc` introduces **HandleScope**, a V8-inspired explicit rooting mechanism. While not required (the GC will automatically fallback to **Conservative Stack Scanning** if you don't use it), HandleScopes provide:

1.  **Maximum Performance**: Reduces GC pause times by providing an explicit list of roots, skipping expensive stack/register scanning where possible.
2.  **Compile-time Safety**: `Handle<'scope, T>` is bound to the scope's lifetime, preventing use-after-scope bugs at compile time.
3.  **Precise Rooting**: Eliminates "false positives" often found in conservative scanning.

### Synchronous Usage

```rust
use rudo_gc::handles::HandleScope;
use rudo_gc::heap::current_thread_control_block;

fn process() {
    let tcb = current_thread_control_block().unwrap();
    let scope = HandleScope::new(&tcb);
    
    let gc = Gc::new(MyData { value: 42 });
    let handle = scope.handle(&gc);
    
    // Use handle like a reference (implements Deref)
    println!("Value: {}", handle.value);
} // Handle automatically becomes invalid here
```

### Asynchronous Usage (Across Await Points)

Standard `HandleScope` handles cannot live across `.await` points. For async code, use `AsyncHandleScope` or the `spawn_with_gc!` macro:

```rust
use rudo_gc::spawn_with_gc;

async fn async_task(data: Gc<MyData>) {
    // Automatically creates an AsyncHandleScope for the spawned task
    spawn_with_gc!(data => |handle| {
        tokio::task::yield_now().await; // Safe to await!
        println!("Still valid: {}", handle.get().value);
    }).await.unwrap();
}
```

### Safe AsyncHandle Access

`AsyncHandle::get()` now performs automatic runtime validation to detect use-after-free bugs:

```rust
spawn_with_gc!(gc => |handle| {
    // Safe: automatically validates scope is still alive
    // Panics with clear error if scope was dropped
    println!("{}", handle.get().value);
});
```

For performance-critical code where you can prove the scope is still alive, use `get_unchecked()`:

```rust
// Unsafe: caller must ensure scope is alive
// Faster: skips scope validation overhead
let value = unsafe { handle.get_unchecked().value };
```

### GcScope - Dynamic GC Tracking

For dynamic scenarios where the number or types of GC objects aren't known at compile time, use `GcScope`:

```rust
use rudo_gc::handles::GcScope;

let gc_a = Gc::new(Data { value: 1 });
let gc_b = Gc::new(Data { value: 2 });

let mut scope = GcScope::new();
scope.track(&gc_a).track(&gc_b);

let result = scope.spawn(|handles| async move {
    let mut sum = 0;
    for handle in handles {
        if let Some(data) = handle.downcast_ref::<Data>() {
            sum += data.value;
        }
    }
    sum
}).await;
```

**When to use GcScope vs spawn_with_gc!:**

| Scenario | Recommended API |
|----------|----------------|
| Known at compile time | `spawn_with_gc!(gc_a, gc_b => |ha, hb\| {})` |
| Dynamic/heterogeneous | `GcScope::new().track().spawn()` |
| Large numbers of GC objects | `GcScope::track_slice(&vec)` |

### AsyncGcHandle - Type-Erased Handles

`GcScope::spawn()` returns `Vec<AsyncGcHandle>`, which provides type-erased access with safe downcasting:

```rust
scope.spawn(|handles| async move {
    for handle in handles {
        // Check type and access safely
        if let Some(data) = handle.downcast_ref::<MyType>() {
            println!("Found MyType: {}", data.field);
        }
    }
}).await;

// Get the type ID for custom handling
let type_id = handle.type_id();
```

### Long-running Loops

If your code has long-running loops that don't perform allocations, use `safepoint()` to ensure threads respond to GC requests:

```rust
use rudo_gc::safepoint;

for _ in 0..1_000_000 {
    // Compute-intensive work without allocations
    let result = heavy_calculation();

    // Check for GC requests to allow timely collection
    safepoint();
}
```

## Handling Cycles

rudo-gc handles cycles automatically when they become unreachable. However, constructing self-referential cycles requires a specific pattern using `Gc::new_cyclic_weak`.

```rust
use rudo_gc::{Gc, Trace, Weak, cell::GcCell};

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Weak<Node>>>,
    data: i32,
}

// Construct a cycle where the node holds a weak reference to itself
let node = Gc::new_cyclic_weak(|weak_self| {
    Node {
        self_ref: GcCell::new(Some(weak_self)),
        data: 42,
    }
});

// Access self through upgrade()
let weak = node.self_ref.borrow();
let self_ref = weak.as_ref().unwrap().upgrade().unwrap();
assert_eq!(self_ref.data, 42);
```

## Tokio Async Integration

Enable the `tokio` feature for async/await support:

```toml
[dependencies]
rudo-gc = { version = "0.7", features = ["derive", "tokio"] }
```

### Root Guards

When accessing `Gc<T>` inside `tokio::spawn`, you must register it as a root using `root_guard()`:

```rust
use rudo_gc::{Gc, Trace, GcTokioExt};

#[derive(Trace)]
struct Data { value: i32 }

async fn example() {
    let gc = Gc::new(Data { value: 42 });

    // Register as root before spawning
    let _guard = gc.root_guard();

    tokio::spawn(async move {
        println!("{}", gc.value); // Safe to access
    }).await.unwrap();
}
```

### GC Safepoints

Use `yield_now()` to allow the GC to run during long computations:

```rust
async fn process_large_dataset(gc: Gc<LargeDataSet>) {
    for item in dataset.iter() {
        // Process item
        gc.yield_now().await; // Allow GC to run
    }
}
```

### Runtime Initialization

Use `#[gc::main]` to automatically initialize the root set before your main function:

```rust
#[gc::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let gc = Gc::new(Data { value: 42 });
    let _guard = gc.root_guard();

    tokio::spawn(async move {
        println!("{}", gc.value);
    }).await.unwrap();
}
```

## Architecture

`rudo-gc` is designed with performance and Rust compatibility in mind:

1.  **Allocation**: Uses thread-local bump-pointer allocation (TLAB) within size-class segments.
2.  **Marking**: Employs a parallel-ready mark-sweep algorithm with work-stealing.
3.  **Sweeping**: By default, uses lazy sweep to defer reclamation to allocation time, reducing STW pauses. Eager sweeping is available when the `lazy-sweep` feature is disabled.
4.  **Lazy Sweep**: Pages with dead objects are marked during collection but swept lazily during subsequent allocations. This amortizes sweep work across allocations, reducing pause times.
4.  **Generations**: Objects start in "Generation 0" and are promoted to "Generation 1" if they survive a Minor GC.
5.  **Interior Mutability**: `GcCell<T>` provides a `RefCell`-like API with integrated write barriers to track old-to-young pointers. Supports both generational and incremental (SATB) barriers.
6.  **Incremental Marking**: Reduces major GC pause times by splitting the mark phase into cooperative increments. Uses hybrid SATB + Dijkstra insertion barrier approach.
7.  **Safe Points**: Cooperative rendezvous protocol for multi-threaded GC coordination. Use `safepoint()` in long-running loops.
8.  **Thread Safety**: Multi-threaded GC with thread coordination and parallel marking.

## Trace Trait

The `Trace` trait is the heart of the collector's safety. It allows the GC to traverse the object graph. `rudo-gc` provides:

- `#[derive(Trace)]` for automatic implementation on custom structs and enums.
- Implementations for standard library types: `Vec`, `HashMap`, `Option`, `Box`, `Rc`, `Arc`, and more.
- Thread-local metrics to monitor GC performance.

For a deeper dive into the philosophy behind the collector, see the [design documents](docs/2026-01-01_22-27-34_Gemini_Google_Gemini.md).

## Safety & Limitations

- **Thread Safety**: `Gc<T>` implements `Send` and `Sync` when `T: Send + Sync`. For non-Send types that need cross-thread communication, use `GcHandle<T>` to safely transfer references between threads without requiring the type itself to be thread-safe.
- **Address Stability**: While objects don't move, their memory is reclaimed once unreachable. Holding an `&T` across a collection point is safe as long as the parent `Gc<T>` is still rooted.
- **Platform Support**: Conservative stack scanning is currently supported on **x86_64 Linux, macOS, and Windows**, as well as **aarch64 Linux**. Miri is also fully supported for testing.
- **Conservative Stack Scanning**: Roots are discovered by scanning the stack and registers. This may cause false positives (integers mistaken as pointers), leading to memory bloat. It also prevents implementing moving/compacting GC.
- **GC Response in Loops**: Threads must call `safepoint()` or perform allocations regularly. Long-running loops without these calls may delay GC response, potentially affecting collection latency.
- **Tokio Roots**: When using the `tokio` feature, roots must be registered with `GcRootSet` via `root_guard()`. Tokio tasks don't share stack with the main thread, so automatic stack scanning won't find roots in spawned tasks.
- **Thread Safety Considerations**: When using `Gc<T>` with third-party libraries like tokio or rayon, ensure proper root registration. Loops that perform heavy computation without allocations should call `safepoint()` periodically.

## License

This project is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## When to Use Gc<T>

Gc<T> is well-suited for:
- Complex cyclic data structures
- Graphs and trees with arbitrary sharing
- Scenarios where manual Weak<T> management is error-prone
- Reactive frameworks with non-Send signal types (via `GcHandle<T>`)

Consider alternatives when:
- Data size is small and predictable (consider Rc/Arc)
- Maximum performance is critical and cycles are unlikely
- Tight loops cannot include safepoint() calls or allocations

## Common Pitfall: Vec<Gc<T>>

**⚠️ Important**: Using `Vec<Gc<T>>` directly can cause memory corruption!

### The Problem

```rust
// ❌ DANGEROUS: May cause memory corruption!
let items: RefCell<Vec<Gc<Component>>> = RefCell::new(Vec::new());
```

The `Vec` is stored in standard Rust allocation (not GC-managed). When GC runs:
1. Stack scanning doesn't find the Vec's contents
2. Write barriers don't mark the Vec's buffer as dirty
3. Gc pointers inside the Vec are not marked as reachable
4. Objects are incorrectly swept and their memory is reused
5. **Result**: Memory corruption with dangling pointers

### The Solution

**Use `Gc<Vec<Gc<T>>>` instead:**

```rust
// ✅ SAFE: Vec is GC-managed, will be traced correctly
let items: Gc<RefCell<Vec<Gc<Component>>>> = Gc::new(RefCell::new(Vec::new()));

// Or with GcCell for mutation:
#[derive(Trace, GcCell)]
struct GameState {
    entities: Vec<Gc<Entity>>,
}

let state: Gc<GcCell<GameState>> = Gc::new(GcCell::new(GameState {
    entities: Vec::new(),
}));
```

### Why This Works

- `Gc<Vec<Gc<T>>>` is a GC-managed object
- It has a `Trace` implementation that traverses all elements
- When GC marks the Vec, it finds and marks all contained `Gc<T>` objects
- Write barriers correctly track the Vec's page as dirty

### For Testing (test-util feature)

For tests using `Vec<Gc<T>>` without `Gc<Vec<Gc<T>>>`, you can manually register the Vec's buffer:

```rust
use rudo_gc::gc::register_test_root_region;

let items: RefCell<Vec<Gc<Component>>> = RefCell::new(Vec::with_capacity(1000));

// Register the Vec's buffer for conservative scanning during GC
register_test_root_region(
    items.borrow().as_ptr() as *const u8,
    items.borrow().capacity() * std::mem::size_of::<Gc<Component>>()
);

// Re-register after pushes (Vec may reallocate)
```

**Note**: The `test-util` feature is required for this API.

## Future: Automatic Detection of Vec<Gc<T>> Issues

**Planned Feature**: Runtime detection and panic for suspicious GC sweeps.

### Goal

Automatically detect when young generation objects are being incorrectly swept and provide a helpful error message pointing to `Vec<Gc<T>>` as the likely cause.

### Implementation Plan

1. **Young Object History Tracking**
   - Record recently created GC objects in a circular buffer
   - Track pointer + creation round for each object
   - Low overhead: ~1024 entries, O(1) insert

2. **Suspicious Sweep Detection**
   - During sweep phase, check if a young object is being collected
   - Cross-reference with young object history
   - If recently created object is being swept → panic with hint

3. **Helpful Error Message**

```
Thread 'main' panicked at 'rudo-gc detected suspicious GC behavior:

A young generation object (ptr=0x600000000668) was not marked but is being swept.
This typically indicates Vec<Gc<T>> was used without Gc<Vec<Gc<T>>>.

Solution:
  Change: let items: RefCell<Vec<Gc<T>>> = ...
  To:     let items: Gc<RefCell<Vec<Gc<T>>>> = Gc::new(RefCell::new(Vec::new()));

For more information, see: crates/rudo-gc/docs/vec-gc-usage.md

This check only runs in debug builds. Enable 'debug-suspicious-sweep' feature for release builds.
'
```

### Configuration

| Feature | Behavior |
|---------|----------|
| Default (debug builds) | Detection enabled, helpful panic |
| `debug-suspicious-sweep` | Enable in release builds |
| `paranoid-sweep` | More aggressive detection (may have false positives) |

### Status

- [ ] YoungObjectHistory tracking in Gc::new()
- [ ] Sweep phase detection hook
- [ ] Helpful panic message
- [ ] Feature flags
- [ ] Documentation

Contributions welcome!
