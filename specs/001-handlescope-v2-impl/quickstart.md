# Quick Start: HandleScope v2

**Feature**: HandleScope v2 Implementation | **Date**: 2026-02-01

## Overview

HandleScope v2 provides compile-time safe garbage-collected handles for rudo-gc. Handles are lifetime-bound to their scope, preventing dangling references at compile time.

## Basic Usage

### Creating a HandleScope

```rust
use rudo_gc::{Gc, Trace, heap::ThreadControlBlock};

fn example(tcb: &mut ThreadControlBlock) {
    // Create a HandleScope - all handles created within are valid until scope ends
    let scope = Handlecb);

    //Scope::new(t Allocate a GC object
    let gc = Gc::new(42);

    // Create a handle bound to the scope
    let handle = scope.handle(&gc);

    // Use the handle (dereferences to &i32)
    println!("Value: {}", *handle);

    // Scope ends here - handle becomes invalid
}
```

### Nested Scopes

```rust
fn nested_example(tcb: &mut ThreadControlBlock) {
    let outer_scope = HandleScope::new(tcb);

    let outer_gc = Gc::new("outer");
    let outer_handle = outer_scope.handle(&outer_gc);

    {
        let inner_scope = HandleScope::new(tcb);

        let inner_gc = Gc::new("inner");
        let inner_handle = inner_scope.handle(&inner_gc);

        // Both handles are valid here
        println!("Outer: {}, Inner: {}", *outer_handle, *inner_handle);

        // inner_handle becomes invalid here
    }

    // outer_handle is still valid
    println!("{}", *outer_handle);
}
```

## Escape Pattern

### Returning a Handle from a Function

```rust
fn create_value<'outer>(
    tcb: &mut ThreadControlBlock,
) -> Handle<'outer, i32> {
    let escape_scope = EscapeableHandleScope::new(tcb);

    let gc = Gc::new(42);
    let inner_handle = escape_scope.handle(&gc);

    // Escape the handle to the outer scope
    escape_scope.escape(inner_handle)
}
```

### Single Escape Constraint

```rust
let escape_scope = EscapeableHandleScope::new(tcb);
let h1 = escape_scope.handle(&Gc::new(1));
let h2 = escape_scope.handle(&Gc::new(2));

// First escape works
let escaped1 = escape_scope.escape(h1);

// Second escape panics!
let escaped2 = escape_scope.escape(h2); // panics
```

## Debug Sealing

### Preventing Handle Creation

```rust
#[cfg(debug_assertions)]
fn sensitive_code(tcb: &mut ThreadControlBlock) {
    let _seal = SealedHandleScope::new(tcb);

    // This will panic in debug mode
    // let handle = scope.handle(&gc);
}
```

In release mode, `SealedHandleScope` is a no-op with zero overhead.

## Async/Await Support

### AsyncHandleScope

```rust
async fn async_example(tcb: Arc<ThreadControlBlock>) {
    let scope = AsyncHandleScope::new(&tcb);

    let gc = Gc::new(42);
    let handle = scope.handle(&gc);

    // Handle remains valid across await points
    some_async_operation().await;

    println!("{}", *handle);
}
```

### spawn_with_gc! Macro

```rust
use rudo_gc::{Gc, Trace, spawn_with_gc};

let gc = Gc::new(MyData { value: 42 });

spawn_with_gc!(gc => |handle| async move {
    println!("{}", handle.get().value);
    some_async_op().await;
    println!("{}", handle.get().value);
});
```

## MaybeHandle Pattern

### Optional Handles

```rust
fn try_create<'scope, T: Trace>(
    scope: &HandleScope<'scope>,
    condition: bool,
    value: T,
) -> MaybeHandle<'scope, T> {
    if condition {
        let gc = Gc::new(value);
        MaybeHandle::from_handle(scope.handle(&gc))
    } else {
        MaybeHandle::empty()
    }
}
```

## Migration from v1

### Before (v1 - Conservative Scanning)

```rust
// v1: Implicit root tracking via conservative stack scanning
fn example() {
    let gc = Gc::new(42);
    // gc is tracked by conservative scanning
}
```

### After (v2 - HandleScope)

```rust
// v2: Explicit HandleScope
fn example(tcb: &mut ThreadControlBlock) {
    let scope = HandleScope::new(tcb);

    let gc = Gc::new(42);
    let handle = scope.handle(&gc);
    // handle is explicitly tracked
}
```

### Async Migration

```rust
// v1: Manual root_guard (error-prone)
let gc = Gc::new(42);
tokio::spawn(async move {
    let _guard = gc.root_guard();  // Easy to forget!
    // ...
});

// v2: spawn_with_gc! (automatic, safe)
let gc = Gc::new(42);
spawn_with_gc!(gc => |handle| async move {
    // handle is automatically tracked
});
```

## Common Patterns

### Factory Function

```rust
fn create_node<'scope, T: Trace>(
    scope: &HandleScope<'scope>,
    value: T,
) -> Handle<'scope, Node<T>> {
    let escape_scope = EscapeableHandleScope::new(&scope.tcb);

    let gc = Gc::new(Node { value, next: None });
    let handle = escape_scope.handle(&gc);

    escape_scope.escape(handle)
}
```

### Conditional Handle Creation

```rust
fn maybe_create<'scope>(
    scope: &HandleScope<'scope>,
    condition: bool,
) -> MaybeHandle<'scope, i32> {
    if condition {
        let gc = Gc::new(42);
        MaybeHandle::from_handle(scope.handle(&gc))
    } else {
        MaybeHandle::empty()
    }
}
```

### Loop with Handles

```rust
fn process_items<'scope>(
    scope: &HandleScope<'scope>,
    items: &[i32],
) {
    for &item in items {
        let gc = Gc::new(item);
        let handle = scope.handle(&gc);
        process(*handle);
    }
}
```

## Error Handling

### HandleScope API Errors

| Scenario | Behavior |
|----------|----------|
| Double escape | `panic!` with message |
| Block exhausted | New block allocated automatically |
| Invalid TCB | Caller error (must provide valid TCB) |

### AsyncHandle Safety

| Scenario | Behavior |
|----------|----------|
| Use after scope drop | Undefined behavior (documented unsafe) |
| Scope in different task | Valid (Arc<TCB> persists) |

## Performance Characteristics

| Operation | Complexity | Notes |
|-----------|------------|-------|
| `HandleScope::new()` | O(1) | Stores prev pointers only |
| `scope.handle()` | O(1) | Bump allocation |
| `Handle::get()` | O(1) | Single pointer read |
| `escape()` | O(1) | Slot copy |
| GC iteration | O(handle_count) | Precise, not conservative |

## Memory Overhead

| Type | Size |
|------|------|
| `Handle<'scope, T>` | 8 bytes |
| `HandleSlot` | 8 bytes |
| `HandleBlock` | 2048 bytes (256 slots) |
| `HandleScopeData` | 24 bytes |

## Feature Flags

```toml
[features]
default = ["handle-scope"]
handle-scope = []           # Enable HandleScope v2
conservative-fallback = []  # Use v1 conservative scanning as backup
async = ["tokio"]           # Enable async support
```

## Testing

Run tests with single thread to avoid GC interference:

```bash
cargo test -- --test-threads=1
```

Run Miri for unsafe code validation:

```bash
./miri-test.sh
```
