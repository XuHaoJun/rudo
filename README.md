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
- **Large Object Space (LOS)**: Specialized handling for objects larger than 2KB to prevent fragmentation.
- **Weak References**: Support for `Weak<T>` pointers with proper lifecycle management.
- **ZST Optimization**: Zero-Sized Types (like `()`) are handled with zero heap allocation overhead.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
rudo-gc = "0.1.0"
```

If you want to use the `#[derive(Trace)]` macro, enable the `derive` feature:

```toml
[dependencies]
rudo-gc = { version = "0.1.0", features = ["derive"] }
```

## Quick Start

```rust
use rudo_gc::{Gc, Trace, cell::GcCell};

// Simple allocation
let x = Gc::new(42);
println!("Value: {}", *x);

// Custom types with derive
#[derive(Trace)]
struct Node {
    value: i32,
    next: GcCell<Option<Gc<Node>>>, // Use GcCell for interior mutability
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

## Architecture

`rudo-gc` is designed with performance and Rust compatibility in mind:

1.  **Allocation**: Uses thread-local bump-pointer allocation (TLAB) within size-class segments.
2.  **Marking**: Employs a parallel-ready mark-sweep algorithm.
3.  **Sweeping**: Reclaims memory into free lists for small objects or deallocates pages for large ones.
4.  **Generations**: Objects start in "Generation 0" and are promoted to "Generation 1" if they survive a Minor GC.
5.  **Interior Mutability**: `GcCell<T>` provides a `RefCell`-like API with integrated write barriers to track old-to-young pointers.

## Trace Trait

The `Trace` trait is the heart of the collector's safety. It allows the GC to traverse the object graph. `rudo-gc` provides:

- `#[derive(Trace)]` for automatic implementation on custom structs and enums.
- Implementations for standard library types: `Vec`, `HashMap`, `Option`, `Box`, `Rc`, `Arc`, and more.
- Thread-local metrics to monitor GC performance.

For a deeper dive into the philosophy behind the collector, see the [design documents](docs/2026-01-01_22-27-34_Gemini_Google_Gemini.md).

## Safety & Limitations

- **Single-threaded**: Currently, `Gc<T>` is `!Send` and `!Sync`. All GC operations are thread-local.
- **Address Stability**: While objects don't move, their memory is reclaimed once unreachable. Holding an `&T` across a collection point is safe as long as the parent `Gc<T>` is still rooted.
- **Platform Support**: Conservative stack scanning is currently supported on **x86\_64 Linux, macOS, and Windows**, as well as **aarch64 Linux**. Miri is also fully supported for testing.

## License

This project is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
