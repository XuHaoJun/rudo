# rudo-gc

A garbage-collected smart pointer library for Rust with automatic cycle detection.

## Features

- **`Gc<T>` smart pointer**: Similar to `Rc<T>`, but with automatic cycle detection
- **BiBOP memory layout**: O(1) allocation using size-class based segments
- **Mark-Sweep collection**: Non-moving GC that preserves Rust's address stability
- **`#[derive(Trace)]`**: Easy integration for custom types
- **Configurable collection**: Control when garbage collection runs

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
rudo-gc = "0.1"
```

Basic usage:

```rust
use rudo_gc::{Gc, Trace};

// Simple allocation
let x = Gc::new(42);
println!("Value: {}", *x);

// Shared ownership
let y = Gc::clone(&x);
assert!(Gc::ptr_eq(&x, &y));

// Custom types with derive
#[derive(Trace)]
struct Node {
    value: i32,
    next: Option<Gc<Node>>,
}

let node = Gc::new(Node { value: 1, next: None });
```

## Handling Cycles

Unlike `Rc<T>`, `Gc<T>` can collect cyclic references:

```rust
use rudo_gc::{Gc, Trace, collect};
use std::cell::RefCell;

#[derive(Trace)]
struct Node {
    next: RefCell<Option<Gc<Node>>>,
}

let a = Gc::new(Node { next: RefCell::new(None) });
let b = Gc::new(Node { next: RefCell::new(None) });

// Create cycle: a -> b -> a
*a.next.borrow_mut() = Some(Gc::clone(&b));
*b.next.borrow_mut() = Some(Gc::clone(&a));

drop(a);
drop(b);
collect(); // Cycle is detected and freed
```

## API Overview

### Types

- `Gc<T>`: Garbage-collected smart pointer
- `Trace`: Trait for types that can be traced by the GC
- `Visitor`: Trait for traversing the object graph
- `CollectInfo`: Statistics about heap state

### Functions

- `collect()`: Force immediate garbage collection
- `set_collect_condition(f)`: Set custom collection trigger
- `default_collect_condition(info)`: The default trigger (amortized O(1))

### Gc<T> Methods

- `Gc::new(value)`: Create a new Gc
- `Gc::new_cyclic(f)`: Create a self-referential Gc
- `Gc::clone(&gc)`: Create another reference
- `Gc::ptr_eq(&a, &b)`: Check if two Gcs point to the same allocation
- `Gc::ref_count(&gc)`: Get current reference count
- `Gc::is_dead(&gc)`: Check if Gc is dead (during Drop)
- `Gc::try_deref(&gc)`: Fallible dereference
- `Gc::try_clone(&gc)`: Fallible clone

## Thread Safety

`Gc<T>` is `!Send` and `!Sync` - it can only be used within a single thread.
Each thread has its own heap and garbage collector.

## Design

This library is inspired by:

- **[dumpster](https://crates.io/crates/dumpster)**: API design and `Trace` trait
- **Chez Scheme**: BiBOP memory layout and Mark-Sweep algorithm
- **John McCarthy's GC principles**: The foundational ideas from LISP

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    User Code                             │
│   Gc::new(val)  →  allocate in heap                     │
│   drop(gc)      →  may trigger collection               │
└─────────────────────────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────┐
│                   GlobalHeap                             │
│   ┌─────────┐ ┌─────────┐ ┌─────────┐                   │
│   │ Seg<16> │ │ Seg<32> │ │ Seg<64> │ ...              │
│   └─────────┘ └─────────┘ └─────────┘                   │
│         │           │           │                        │
│         ▼           ▼           ▼                        │
│   ┌─────────┐ ┌─────────┐ ┌─────────┐                   │
│   │  Page   │ │  Page   │ │  Page   │                   │
│   │ Header  │ │ Header  │ │ Header  │                   │
│   │ + Slots │ │ + Slots │ │ + Slots │                   │
│   └─────────┘ └─────────┘ └─────────┘                   │
└─────────────────────────────────────────────────────────┘
```

## License

MIT OR Apache-2.0
