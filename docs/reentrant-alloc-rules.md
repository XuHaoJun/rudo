# Reentrant Allocation Rules

This document defines rules for handling allocation that occurs during garbage collection (GC) cycles, specifically when user code (such as `Drop` implementations) triggers memory allocation while the GC is actively iterating over heap pages.

## Why This Matters

### Iterator Invalidation (Undefined Behavior)

When GC Phase 1 iterates over `heap.pages` and executes user `drop_fn`, if `drop_fn` allocates new memory, it calls `alloc_slow`, which modifies `heap.pages` by pushing a new page header. This invalidates the active iterator:

```rust
// UNSAFE: Iterator invalidation
for page_ptr in heap.all_pages() {          // Iterator created
    drop_fn_might_allocate();                // Modifies heap.pages (UB!)
}
```

**Consequences:**
- Dangling pointer to old `Vec` buffer after reallocation
- Reading garbage data from freed memory
- Potential segfault or silent memory corruption

### Use-After-Free in Drop

If an object is dropped twice (because `set_dead()` was forgotten), the second drop accesses freed memory:

```rust
// Problem: is_value_dead() returns false, drop runs again
if weak_count == 0 && (*gc_box_ptr).is_value_dead() {
    ((*gc_box_ptr).drop_fn)(obj_ptr);  // Use-after-free!
}
```

## Rules Summary

| Path | User Code Runs? | Reentrant Alloc | Safety Mechanism |
|------|-----------------|-----------------|------------------|
| GC Phase 1 (`sweep_phase1_finalize`) | Yes | ⚠️ Allowed | Snapshot pages first |
| GC Phase 2 (`sweep_phase2_reclaim`) | No | ❌ Forbidden | No user code |
| `alloc_slow` | No | ❌ Forbidden during GC | Modifies `heap.pages` |
| Fast path (`alloc`, `alloc_tlab`) | No | ✅ Allowed | Normal operation |

## Detailed Rules with Examples

### Rule 1: GC Phase 1 - Snapshot Before Iteration

When executing user `drop_fn` during sweep, always snapshot pages first:

```rust
// SAFE: Snapshot pages before iteration
let pages_snapshot: Vec<_> = heap.all_pages().collect();

for page_ptr in pages_snapshot {
    unsafe {
        // User code runs here - can allocate new GC objects
        // New pages are added to heap.pages, but pages_snapshot is independent
        ((*gc_box_ptr).drop_fn)(obj_ptr);
    }
}
```

**Why this works:** The snapshot creates an independent `Vec` of page pointers. Even if `drop_fn` triggers allocation and modifies `heap.pages`, the snapshot remains valid.

### Rule 2: `alloc_slow` - Never Call During Page Iteration

The slow path for allocation directly modifies the heap's page collection:

```rust
// UNSAFE: Called during page iteration
self.pages.push(header);       // Modifies Vec being iterated
self.small_pages.insert(ptr); // Modifies HashSet being iterated

// SAFE: Called outside GC critical section (normal allocation)
```

**Rule:** If you are iterating pages, do not call `alloc_slow` or any function that modifies `heap.pages`.

### Rule 3: Weak References - Preserve Allocation

Objects with weak references must keep their allocation alive even after the value is dropped:

```rust
if weak_count > 0 {
    // Drop the value (user code runs)
    if !(*gc_box_ptr).is_value_dead() {
        ((*gc_box_ptr).drop_fn)(obj_ptr);
        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
        (*gc_box_ptr).set_dead();
    }
    // But keep allocation alive - weak ref might upgrade later
}
```

### Rule 4: Always Mark Objects as Dead

After executing `drop_fn`, mark the object as dead to prevent double-drop:

```rust
// SAFE: Mark as dead after drop
((*gc_box_ptr).drop_fn)(obj_ptr);
(*gc_box_ptr).set_dead();  // Prevents double-drop in Phase 2

// UNSAFE: Forgot to mark dead - Phase 2 might drop again
```

## Key Functions Reference

### `alloc_slow`

**Safety Status:** NOT reentrant-safe during GC

**Modifies:**
- `self.pages.push(header)`
- `self.small_pages.insert(ptr)`

**Never call from:** Functions that iterate `heap.pages`

### `sweep_phase1_finalize`

**Safety Status:** Executes user code (drop_fn)

**Requirement:** Must snapshot pages before iteration

**Pattern:**
```rust
let pages_snapshot: Vec<_> = heap.all_pages().collect();
for page_ptr in pages_snapshot { ... }
```

### `sweep_phase2_reclaim`

**Safety Status:** Safe (no user code execution)

**Note:** Although allocation is theoretically possible here, the phase operates on bitmap state only and does not iterate pages in a way that would conflict with allocation.

## Modification Checklist

When modifying GC or allocation code, verify:

- [ ] **If iterating pages** → Snapshot pages first
- [ ] **If executing user code** → Allocation may happen (use snapshot)
- [ ] **If calling `alloc_slow`** → Ensure NOT during page iteration
- [ ] **If modifying bitmaps** → Safe (no heap.pages modification)
- [ ] **After `drop_fn`** → Always call `set_dead()`
- [ ] **With weak refs** → Keep allocation alive after value drop

## Test Reference

### `test_drop_allocates`

Verifies that the snapshot pattern works correctly - `drop_fn` can safely allocate new objects while the original page iteration continues using the snapshot.

```rust
#[test]
fn test_drop_allocates() {
    // Drop implementation allocates new GC objects
    impl Drop for AllocatingDropper {
        fn drop(&mut self) {
            let _ = Gc::new(12345i32);  // Triggers alloc_slow
        }
    }

    // Snapshot prevents iterator invalidation
    let pages_snapshot: Vec<_> = heap.all_pages().collect();
    for page_ptr in pages_snapshot {
        // Drop runs safely even though heap.pages changes
    }
}
```

## Common Mistakes

### Mistake 1: Forgetting Snapshot

```rust
// WRONG: Direct iteration
for page_ptr in heap.all_pages() {
    ((*gc_box_ptr).drop_fn)(obj_ptr);  // May allocate!
}
```

### Mistake 2: Forgetting set_dead()

```rust
// WRONG: Object marked dead twice in Phase 2
((*gc_box_ptr).drop_fn)(obj_ptr);
// Missing: (*gc_box_ptr).set_dead();
```

### Mistake 3: Clearing Allocated Bit Too Early

```rust
// WRONG: Cleared before Phase 2 can check it
if !is_marked(i) && is_allocated(i) {
    ((*gc_box_ptr).drop_fn)(obj_ptr);
    (*header).clear_allocated(i);  // Phase 2 can't detect dead objects!
}
```

## Related Documentation

- `docs/two-phase-sweep-fix-plan-1.md` - Original issue analysis
- `docs/bug-4.md` - Large object map consistency
- `docs/lazy-sweep-plan.md` - Future lazy sweep considerations
