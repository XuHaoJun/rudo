# Quickstart: Cross-Thread GC Handles

This guide covers how to use `GcHandle` and `WeakCrossThreadHandle` to safely pass GC-managed objects between threads.

## Overview

Cross-thread handles allow you to reference GC objects from any thread without requiring the objects themselves to implement `Send` or `Sync`. Handles can be sent through channels, spawned in tasks, or stored in other threads, but can only be resolved back to `Gc<T>` on the thread where they were created.

## Creating Handles

### Strong Handle

Create a strong cross-thread handle that keeps the object alive:

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct SignalData {
    value: i32,
}

let gc: Gc<SignalData> = Gc::new(SignalData { value: 42 });
let handle: GcHandle<SignalData> = gc.cross_thread_handle();
```

### Weak Handle

Create a weak handle that doesn't prevent collection:

```rust
let weak_handle: WeakCrossThreadHandle<SignalData> = gc.weak_cross_thread_handle();
```

## Sending Between Threads

Handles are `Send + Sync` regardless of the inner type:

```rust
use std::thread;
use std::sync::mpsc;

let (sender, receiver) = mpsc::channel::<GcHandle<SignalData>>();

// Spawn a thread that sends the handle back
thread::spawn(move || {
    sender.send(handle).unwrap();
});

// Receive on the origin thread
let received_handle = receiver.recv().unwrap();
```

## Resolving Handles

### Blocking Resolve

Resolves only on the origin thread:

```rust
// This works on the origin thread
let signal: Gc<SignalData> = handle.resolve();

// This panics (wrong thread)
let foreign_handle = received_handle;
let _ = foreign_thread.spawn(move || {
    // This will panic!
    let _ = foreign_handle.resolve();
});
```

### Try Resolve (Graceful)

Use `try_resolve()` when you might be on the wrong thread:

```rust
if let Some(signal) = handle.try_resolve() {
    // We're on the origin thread, safely access the object
    signal.value += 1;
} else {
    // We're on the wrong thread - queue for later
    other_thread_sender.send(handle.clone());
}
```

## Checking Validity

```rust
// Check if handle is still registered
if handle.is_valid() {
    let signal = handle.resolve();
    // Use signal...
}
```

## Cloning Handles

Cloning creates an independent root:

```rust
let handle2 = handle.clone();
// Both handle and handle2 keep the object alive
// Dropping one doesn't affect the other
```

## Downgrading to Weak

```rust
let weak = handle.downgrade();
// Weak doesn't keep object alive
// Can upgrade back to strong if object still exists
```

## Explicit Unregistration

Remove the handle from the root set:

```rust
handle.unregister();
// Now is_valid() returns false
// resolve() will panic
// Drop is a no-op
```

## Complete Example: Async UI Updates

This pattern is common in UI frameworks like Rvue:

```rust
use std::sync::mpsc;
use std::thread;

#[derive(Trace)]
struct ReactiveState {
    count: i32,
}

// UI thread: create signal and handle
let state: Gc<ReactiveState> = Gc::new(ReactiveState { count: 0 });
let handle: GcHandle<ReactiveState> = state.cross_thread_handle();

let (update_sender, update_receiver) = mpsc::channel();

// Send handle to worker thread
let worker_sender = update_sender.clone();
thread::spawn(move || {
    for i in 0..10 {
        // Send update to UI thread
        worker_sender.send(i).unwrap();
        thread::sleep(std::time::Duration::from_millis(100));
    }
});

// UI thread: process updates
for value in update_receiver {
    if let Some(state) = handle.try_resolve() {
        state.count = value;
        // Trigger UI update...
    }
    // If try_resolve returns None, the handle wasn't ready yet
}
```

## Thread Exit Behavior

If the origin thread exits while handles still exist:

- `resolve()` will panic (thread no longer exists)
- `try_resolve()` returns `None`
- Object remains alive (roots prevent collection)
- `Drop` remains safe (mutex access only)

Best practice: Resolve or drop handles before origin thread exits.

## Performance Characteristics

| Operation | Performance |
|-----------|-------------|
| `cross_thread_handle()` | Mutex lock + HashMap insert (cold path) |
| `resolve()` / `try_resolve()` | ThreadId comparison + pointer deref (hot path, lock-free) |
| `clone()` | Mutex lock + HashMap insert (cold path) |
| `drop()` | Mutex lock + HashMap remove (cold path) |

## Integration with Existing GC Features

Cross-thread handles work seamlessly with:

- **Incremental Marking**: No additional barriers needed; existing SATB and Dijkstra barriers handle resolved objects correctly
- **Concurrent GC**: Lock-free resolve hot path ensures minimal contention
- **Tracing**: Handles are properly marked during root scanning

## Safety Guarantees

1. Handles are `Send + Sync` even when `T` is not
2. Resolution is enforced at runtime on origin thread only
3. Strong handles prevent collection while registered
4. Drop is safe from any thread
5. No undefined behavior - wrong-thread access results in panic

## Migration from Old Patterns

If you previously couldn't send `Gc<T>` across threads:

**Before** (impossible for `T: !Send`):
```rust
// This fails to compile for T: !Send
let other_thread = thread::spawn(move || {
    process_signal(gc_signal);
});
```

**After** (using handles):
```rust
// Create handle on origin thread
let handle = gc_signal.cross_thread_handle();

// Send handle to other thread
let other_thread = thread::spawn(move || {
    // Can't access directly, but can communicate back
    let (tx, rx) = channel();
    tx.send(handle).unwrap();
    // ... or queue for later resolution on origin thread
});
```

## Common Patterns

### Fire-and-Forget with Weak Handles

```rust
// Create weak handle
let weak = handle.downgrade();

// In worker thread: check if still alive before work
if weak.is_valid() {
    // Object still exists, safe to queue update
    update_sender.send(weak.clone());
}
```

### Batched Updates

```rust
// Collect multiple handles, send to origin thread once
let handles: Vec<GcHandle<Data>> = collect_handles();
channel.send(handles).unwrap();

// On origin thread: resolve all
for handle in handles {
    if let Some(data) = handle.try_resolve() {
        process(data);
    }
}
```

## Testing

Run the test suite to verify correct behavior:

```bash
cargo test --test cross_thread_handle
```

Key tests include:
- `test_cross_thread_send`: Handle transfer between threads
- `test_resolve_origin_thread`: Origin-thread enforcement
- `test_try_resolve_wrong_thread`: Graceful wrong-thread handling
- `test_handle_keeps_alive`: Liveness guarantees
- `test_drop_from_foreign_thread`: Safe cross-thread drop
- `test_miri_thread_safety`: Memory safety verification
