# Quick Start: Send + Sync Trait Support

**Date**: 2026-01-27  
**Feature**: Send + Sync Trait Support (`002-send-sync-trait`)

---

## Overview

This feature enables `Gc<T>` and `Weak<T>` to implement `Send` and `Sync` traits, allowing garbage-collected pointers to be safely shared across threads.

---

## Usage Examples

### Basic Multi-threaded Sharing

```rust
use rudo_gc::{Gc, Trace};
use std::thread;

#[derive(Trace)]
struct SharedData {
    value: i32,
}

fn main() {
    // Create a Gc on the main thread
    let gc = Gc::new(SharedData { value: 42 });
    
    // Clone and send to another thread
    let gc_clone = gc.clone();
    
    thread::spawn(move || {
        // Access the Gc from another thread
        println!("Value from thread: {}", gc_clone.value);
    }).join().unwrap();
}
```

### Concurrent Reference Counting

```rust
use rudo_gc::{Gc, Trace};
use std::sync::{Arc, AtomicUsize};
use std::thread;

#[derive(Trace)]
struct Counter {
    count: Arc<AtomicUsize>,
}

fn main() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    
    for _ in 0..4 {
        let counter_clone = Arc::clone(&counter);
        let gc = Gc::new(Counter { count: counter_clone });
        
        let handle = thread::spawn(move || {
            for _ in 0..1000 {
                gc.clone(); // Concurrent clone
            }
        });
        handles.push(handle);
    }
    
    for handle in handles {
        handle.join().unwrap();
    }
    
    // All clones dropped, counter should be 0
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 0);
}
```

### Weak References Across Threads

```rust
use rudo_gc::{Gc, Weak, Trace};
use std::thread;

#[derive(Trace)]
struct Node {
    value: i32,
    next: Option<Gc<Node>>,
}

fn main() {
    let node = Gc::new(Node { value: 1, next: None });
    let weak = Gc::downgrade(&node);
    
    let handle = thread::spawn(move || {
        // Upgrade weak reference on another thread
        if let Some(upgraded) = weak.upgrade() {
            println!("Value: {}", upgraded.value);
        }
    });
    
    handle.join().unwrap();
}
```

---

## Requirements

- **Rust**: 1.75 or later
- **Feature Flag**: None (enabled by default when trait bounds are satisfied)
- **Platform**: x86_64 or aarch64 on Linux, macOS, Windows

---

## API Changes

### Trait Bounds

| Type | Previous Bounds | New Bounds (for Send/Sync) |
|------|-----------------|---------------------------|
| `Gc<T>` | `T: Trace` | `T: Trace + Send + Sync` |
| `Weak<T>` | `T: Trace` | `T: Trace + Send + Sync` |

### Send/Sync Semantics

- `Gc<T>` is `Send` when `T: Trace + Send + Sync`
- `Gc<T>` is `Sync` when `T: Trace + Send + Sync`
- `Weak<T>` is `Send` when `T: Trace + Send + Sync`
- `Weak<T>` is `Sync` when `T: Trace + Send + Sync`

---

## Performance Characteristics

| Operation | Single-thread | Multi-thread | Notes |
|-----------|---------------|--------------|-------|
| `Gc::new()` | ~100 cycles | ~100 cycles | Allocation dominates |
| `Gc::clone()` | ~5 cycles | ~20-50 cycles | Atomic increment |
| `Gc::drop()` | ~5 cycles | ~25-50 cycles | Atomic decrement + potential collection |
| `Gc::deref()` | ~2 cycles | ~2-5 cycles | Atomic load |

---

## Testing

### Compile-time Verification

```rust
use static_assertions::{assert_impl_all, assert_not_impl_any};

// Verify Send + Sync
assert_impl_all!(Gc<Arc<AtomicUsize>>, Send, Sync);
assert_impl_all!(Weak<Arc<AtomicUsize>>, Send, Sync);

// Verify !Send + !Sync for non-thread-safe types
assert_not_impl_any!(Gc<RefCell<i32>>, Send, Sync);
```

### Run Tests

```bash
# Run all tests including ignored
./test.sh

# Run Miri for memory safety
./miri-test.sh

# Run with ThreadSanitizer
RUSTFLAGS="-Z sanitizer=thread" cargo test --test-threads=1
```

---

## Migration Guide

### From Single-threaded to Multi-threaded

**Before (single-threaded only)**:

```rust
#[derive(Trace)]
struct MyData {
    value: i32,
}

let gc = Gc::new(MyData { value: 42 });
// Cannot send gc to another thread
```

**After (multi-threaded)**:

```rust
#[derive(Trace)]
struct MyData {
    value: i32,  // Must be Send + Sync for Gc to be Send + Sync
}

// Or use Arc for interior types
use std::sync::Arc;

#[derive(Trace)]
struct MyData {
    value: Arc<Mutex<i32>>,
}

let gc = Gc::new(MyData { value: Arc::new(Mutex::new(42)) });
// Now sendable to other threads!
```

---

## Limitations

- **Parallel marking NOT included**: GC collection remains stop-the-world
- **Reference count saturation**: Counts saturate at `isize::MAX`
- **Performance overhead**: Atomic operations have ~4-10x overhead vs Cell

---

## Troubleshooting

### "Gc<T> is not Send"

**Cause**: `T` does not implement `Send + Sync`

**Solution**:
```rust
// Wrap non-Send type in Arc
use std::sync::Arc;

#[derive(Trace)]
struct MyData {
    inner: Arc<RefCell<i32>>,  // RefCell is not Send, but Arc<RefCell<...>> is
}
```

### Data Races Detected

**Cause**: Incorrect memory ordering or unsafe code

**Solution**:
- Ensure all atomic operations use proper ordering
- Add SAFETY comments to unsafe blocks
- Run Miri tests: `./miri-test.sh`

---

## Further Reading

- [Feature Specification](spec.md)
- [Research Notes](research.md)
- [Data Model](data-model.md)
- [rudo-gc Constitution](../../.specify/memory/constitution.md)
