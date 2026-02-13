# Vec<Gc<T>> vs Gc<Vec<Gc<T>>>

## The Problem

In rudo-gc, storing `Gc<T>` pointers in a standard `Vec<Gc<T>>` (or any non-GC container like `RefCell<Vec<Gc<T>>>`) will cause memory issues.

### Why It Fails

When you use `Vec<Gc<T>>`:

```rust
// WRONG - This will cause issues
let items: RefCell<Vec<Gc<i32>>> = RefCell::new(Vec::new());
items.borrow_mut().push(Gc::new(42));
```

1. The `Vec` itself is not managed by the GC
2. The `Gc` pointers inside the `Vec` are invisible to the garbage collector
3. During GC sweep phase, objects referenced only by the `Vec` may be incorrectly collected
4. This leads to dangling pointers and undefined behavior

## The Solution

Wrap the entire container in `Gc<T>`:

```rust
// CORRECT - GC manages the container
let items: Gc<RefCell<Vec<Gc<i32>>>> = Gc::new(RefCell::new(Vec::new()));
items.borrow_mut().push(Gc::new(42));
```

Now:
1. The `Vec` is allocated on the GC heap
2. All `Gc` pointers inside are properly tracked as roots
3. The GC correctly identifies all references and won't collect live objects

## Common Patterns

### Storing Multiple GC Objects

```rust
// Good: Container is GC-managed
let items: Gc<RefCell<Vec<Gc<i32>>>> = Gc::new(RefCell::new(Vec::new()));

// Good: Add items
items.borrow_mut().push(Gc::new(1));
items.borrow_mut().push(Gc::new(2));
```

### Nested Structures

```rust
// Good: Nested GC containers
let outer: Gc<RefCell<Vec<Gc<RefCell<Vec<Gc<i32>>>>>>> = 
    Gc::new(RefCell::new(Vec::new()));
```

### With Custom Types

```rust
#[derive(Trace)]
struct MyData {
    value: i32,
}

let items: Gc<RefCell<Vec<Gc<MyData>>>> = Gc::new(RefCell::new(Vec::new()));
items.borrow_mut().push(Gc::new(MyData { value: 42 }));
```

## Debug Detection

When the `debug-suspicious-sweep` feature is enabled, rudo-gc will panic if it detects a young object being collected that was likely created from this pattern:

```
Thread 'main' panicked at 'rudo-gc detected suspicious GC behavior:

A young generation object was not marked but is being swept.
This typically indicates Vec<Gc<T>> was used without Gc<Vec<Gc<T>>>.

Solution:
  Change: let items: RefCell<Vec<Gc<T>>> = ...
  To:     let items: Gc<RefCell<Vec<Gc<T>>>> = Gc::new(RefCell::new(Vec::new()));
```

## Summary

| Pattern | GC Managed | Safe |
|---------|------------|------|
| `Vec<Gc<T>>` | ❌ | ❌ |
| `RefCell<Vec<Gc<T>>>` | ❌ | ❌ |
| `Gc<RefCell<Vec<Gc<T>>>>` | ✅ | ✅ |
| `Gc<Vec<Gc<T>>>` | ✅ | ✅ |

**Rule of thumb**: If a container holds `Gc<T>` pointers, the container itself must also be a `Gc<T>`.
