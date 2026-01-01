# Quickstart: Rust Dumpster GC (rudo-gc)

**Feature Branch**: `001-rust-dumpster-gc`  
**Date**: 2026-01-02

## Overview

`rudo-gc` is a garbage-collected smart pointer library for Rust, providing automatic memory management with cycle detection. It uses a BiBOP (Big Bag of Pages) memory layout for efficient allocation and a Mark-Sweep algorithm for collection.

---

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rudo-gc = "0.1"
```

To use the derive macro (enabled by default):

```toml
[dependencies]
rudo-gc = { version = "0.1", features = ["derive"] }
```

---

## Basic Usage

### Creating Garbage-Collected Values

```rust
use rudo_gc::Gc;

fn main() {
    // Create a Gc-managed integer
    let x: Gc<i32> = Gc::new(42);
    
    println!("Value: {}", *x);  // Dereference with *
    
    // x is automatically freed when it goes out of scope
}
```

### Shared Ownership

```rust
use rudo_gc::Gc;

fn main() {
    let a = Gc::new(String::from("hello"));
    let b = Gc::clone(&a);  // Clone creates another reference
    
    assert!(Gc::ptr_eq(&a, &b));  // Same allocation
    assert_eq!(Gc::ref_count(&a).get(), 2);
    
    drop(a);
    assert_eq!(Gc::ref_count(&b).get(), 1);
}
```

---

## Custom Types with Trace

To store custom types in a `Gc`, derive the `Trace` trait:

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Person {
    name: String,
    age: u32,
}

fn main() {
    let person = Gc::new(Person {
        name: String::from("Alice"),
        age: 30,
    });
    
    println!("{} is {} years old", person.name, person.age);
}
```

---

## Handling Cycles

Unlike `Rc`, `Gc` can automatically collect cyclic references:

```rust
use rudo_gc::{Gc, Trace, collect};
use std::cell::RefCell;

#[derive(Trace)]
struct Node {
    value: i32,
    next: RefCell<Option<Gc<Node>>>,
}

fn main() {
    let a = Gc::new(Node { value: 1, next: RefCell::new(None) });
    let b = Gc::new(Node { value: 2, next: RefCell::new(None) });
    
    // Create a cycle: a -> b -> a
    *a.next.borrow_mut() = Some(Gc::clone(&b));
    *b.next.borrow_mut() = Some(Gc::clone(&a));
    
    drop(a);
    drop(b);
    
    // Force collection - the cycle is detected and freed
    collect();
}
```

### Self-Referential Structures

Use `Gc::new_cyclic` for structures that reference themselves:

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct SelfRef {
    this: Gc<SelfRef>,
}

fn main() {
    let gc = Gc::new_cyclic(|this| SelfRef { this });
    
    // gc.this points back to gc itself
    assert!(Gc::ptr_eq(&gc, &gc.this));
}
```

---

## Collection Control

### Manual Collection

Force immediate garbage collection:

```rust
use rudo_gc::{Gc, collect};

fn main() {
    {
        let _ = Gc::new(vec![0u8; 1_000_000]);  // Large allocation
    }
    // ^ Gc dropped here, but memory may not be freed yet
    
    collect();  // Now the memory is definitely freed
}
```

### Custom Collection Condition

Control when automatic collection happens:

```rust
use rudo_gc::{set_collect_condition, CollectInfo};

// Never collect automatically (manual only)
set_collect_condition(|_| false);

// Collect every time a Gc is dropped
set_collect_condition(|_| true);

// Collect when heap is "full" (custom heuristic)
set_collect_condition(|info| {
    info.n_gcs_dropped_since_last_collect() > 1000
});
```

---

## Safe Drop Handling

During the Drop of a cycle, some Gc pointers may be "dead". Use safe accessors:

```rust
use rudo_gc::{Gc, Trace};
use std::cell::RefCell;

#[derive(Trace)]
struct Node {
    next: RefCell<Option<Gc<Node>>>,
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(ref next) = *self.next.borrow() {
            // Don't do this - may panic!
            // let _ = &**next;
            
            // Do this instead:
            if let Some(value) = Gc::try_deref(next) {
                println!("Next node exists");
            } else {
                println!("Next node already collected");
            }
        }
    }
}
```

---

## Best Practices

### Do

- ✅ Use `#[derive(Trace)]` for all types stored in `Gc`
- ✅ Use `RefCell` for interior mutability (with `Gc`)
- ✅ Call `collect()` when you need deterministic cleanup
- ✅ Use `Gc::try_deref()` in `Drop` implementations

### Don't

- ❌ Manually implement `Trace` (unless you know what you're doing)
- ❌ Store `Gc` across threads (it's `!Send` and `!Sync`)
- ❌ Assume immediate collection on drop
- ❌ Panic in `Drop` implementations of traced types

---

## Comparison with Other Smart Pointers

| Feature | `Box<T>` | `Rc<T>` | `Arc<T>` | `Gc<T>` |
|---------|----------|---------|----------|---------|
| Heap allocation | ✅ | ✅ | ✅ | ✅ |
| Shared ownership | ❌ | ✅ | ✅ | ✅ |
| Thread-safe | N/A | ❌ | ✅ | ❌ |
| Cycle collection | N/A | ❌ | ❌ | ✅ |
| Deref overhead | None | None | None | None |
| Drop overhead | O(1) | O(1) | O(1) | O(1) amortized |

---

## Troubleshooting

### "Attempt to dereference Gc to already-collected object"

This panic occurs when you dereference a dead `Gc` during a `Drop` implementation. Use `Gc::try_deref()` instead:

```rust
// Before (panics)
let value = &*self.some_gc;

// After (safe)
if let Some(value) = Gc::try_deref(&self.some_gc) {
    // Use value
}
```

### Memory not being freed

Ensure you're not holding onto Gc references. If needed, call `collect()` explicitly:

```rust
use rudo_gc::collect;

fn process_data() {
    // ... create and drop many Gc values ...
}

fn main() {
    process_data();
    collect();  // Ensure cleanup
}
```

---

## Next Steps

- See [API Contracts](./contracts/api.md) for complete API reference
- See [Data Model](./data-model.md) for internal architecture
- See [Research](./research.md) for design decisions
