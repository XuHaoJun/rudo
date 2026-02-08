# Quickstart: Concurrent GC Primitives

**Feature**: 011-concurrent-gc-primitives | **Date**: 2026-02-08

## When to Use Which Type

| Type | Use When |
|------|----------|
| `GcCell<T>` | Single-threaded code only; maximum performance; no synchronization needed |
| `GcRwLock<T>` | Multiple readers, few writers; read-heavy concurrent workloads |
| `GcMutex<T>` | Frequent writes; exclusive access required; write-heavy workloads |

## Basic Usage

### GcRwLock: Shared Cache Pattern

```rust
use rudo_gc::{Gc, GcRwLock};

// Create a shared cache accessible from multiple threads
let shared_cache: Gc<GcRwLock<HashMap<String, String>>> =
    Gc::new(GcRwLock::new(HashMap::new()));

// Reader thread: multiple readers can access concurrently
let reader = {
    let cache = Gc::clone(&shared_cache);
    std::thread::spawn(move || {
        let guard = cache.read();
        if let Some(value) = guard.get("key") {
            println!("Found: {}", value);
        }
    })
};

// Writer thread: exclusive access
let writer = {
    let cache = Gc::clone(&shared_cache);
    std::thread::spawn(move || {
        let mut guard = cache.write();
        guard.insert("key".to_string(), "value".to_string());
    })
};

reader.join().unwrap();
writer.join().unwrap();
```

### GcMutex: Shared Queue Pattern

```rust
use rudo_gc::{Gc, GcMutex};

// Create a thread-safe queue
let queue: Gc<GcMutex<Vec<i32>>> = Gc::new(GcMutex::new(Vec::new()));

// Producer thread
{
    let queue = Gc::clone(&queue);
    std::thread::spawn(move || {
        let mut guard = queue.lock();
        guard.push(42);
        guard.push(100);
    });
}

// Consumer thread
{
    let queue = Gc::clone(&queue);
    std::thread::spawn(move || {
        let mut guard = queue.lock();
        if let Some(value) = guard.pop() {
            println!("Consumed: {}", value);
        }
    });
}
```

## Migration Guide

### From GcCell to GcRwLock/GcMutex

**Single-threaded code remains unchanged:**

```rust
// BEFORE (still works)
let cell: Gc<GcCell<RefCell<MyStruct>>> = Gc::new(GcCell::new(MyStruct::default()));

// AFTER (GcCell unchanged, still preferred for single-threaded)
let cell: Gc<GcCell<MyStruct>> = Gc::new(GcCell::new(MyStruct::default()));
```

**Multi-threaded code adds synchronization:**

```rust
// Single-threaded GcCell (no change needed)
let cell: Gc<GcCell<MyStruct>> = Gc::new(GcCell::new(MyStruct::default()));

// Multi-threaded: wrap in GcRwLock or GcMutex
let shared: Gc<GcRwLock<MyStruct>> = Gc::new(GcRwLock::new(MyStruct::default()));
```

## Performance Characteristics

| Operation | GcCell | GcRwLock | GcMutex |
|-----------|--------|----------|---------|
| `read()` / `borrow()` | O(1), no atomics | O(1), atomic load | N/A |
| `write()` / `borrow_mut()` | O(1), no atomics | O(1), atomic + barrier | O(1), atomic + barrier |
| GC tracing | Direct | Lock bypass | Lock bypass |
| Memory overhead | None | 1 word (lock) | 1 word (lock) |

## Write Barriers

Write barriers are automatically triggered when acquiring mutable guards:

```rust
let lock: Gc<GcRwLock<MyStruct>> = Gc::new(GcRwLock::new(MyStruct::default()));

// Write barrier triggered here (generational + SATB if enabled)
let mut guard = lock.write();
guard.some_field = new_value;
```

Barriers inform the GC of:
1. **Generational**: Which pages were modified (for minor collection targeting)
2. **SATB**: Old value before modification (for incremental marking consistency)

## Error Handling

All lock operations block indefinitely (like std::sync::Mutex):

```rust
let lock: Gc<GcMutex<i32>> = Gc::new(GcMutex::new(0));

// Blocks until lock acquired (no timeout)
let guard = lock.lock();

// For non-blocking access, use try_lock:
if let Some(guard) = lock.try_lock() {
    // Acquired successfully
} else {
    // Lock was held by another thread
}
```

## Thread Safety Verification

Test concurrent access with Miri and ThreadSanitizer:

```bash
# Run with Miri to detect undefined behavior
cargo +nightly miri test --test integration_concurrent

# Run with ThreadSanitizer to detect data races
RUSTFLAGS="-Z sanitizer=thread" cargo test
```

## Common Patterns

### Clone Pattern for Thread Spawning

```rust
let data: Gc<GcRwLock<SharedState>> = Gc::new(GcRwLock::new(SharedState::new()));

for i in 0..4 {
    let data = Gc::clone(&data);
    std::thread::spawn(move || {
        // Each thread has its own Gc pointer
        let guard = data.read();
        // Access shared state...
    });
}
```

### Arc-Like Semantics

```rust
// Gc<T> provides Arc-like cloning for thread-safe sharing
let shared: Gc<GcMutex<SharedData>> = Gc::new(GcMutex::new(SharedData::new()));

// Clone Gc to pass to new thread
let handle = {
    let shared = Gc::clone(&shared);
    std::thread::spawn(move || {
        let guard = shared.lock();
        // Access shared data...
    })
};
```