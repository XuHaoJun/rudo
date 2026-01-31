# API Contracts: Lazy Sweep Public API

## Overview

The lazy sweep feature exposes two public API functions for sweep control and introspection. These functions are only available when the `lazy-sweep` feature is enabled.

## API Reference

### `sweep_pending`

```rust
#[cfg(feature = "lazy-sweep")]
pub fn sweep_pending(num_pages: usize) -> usize
```

**Description**: Triggers lazy sweep on up to `num_pages` pages that are pending sweep.

**Parameters**:
- `num_pages: usize` - Maximum number of pages to sweep

**Returns**: `usize` - Number of pages actually swept

**Behavior**:
1. Iterates through pages needing sweep
2. For each page, calls `lazy_sweep_page()` which processes up to 16 objects
3. Stops when `num_pages` pages have been swept OR no more pages need sweep

**Preconditions**:
- GC runtime must be initialized
- Feature flag `lazy-sweep` must be enabled

**Postconditions**:
- Up to `num_pages` pages have been swept
- Return value indicates how many pages were processed
- Memory reclaimed from swept pages is available for allocation

**Error Conditions**:
- If `num_pages == 0`, returns 0 immediately (no pages swept)
- No error conditions under normal operation

**Thread Safety**: Safe to call from any thread; uses thread-local heap access

**Example Usage**:
```rust
use rudo_gc::sweep_pending;

// Force sweep up to 10 pages before critical operation
let swept = sweep_pending(10);
println!("Swept {} pages", swept);
```

---

### `pending_sweep_pages`

```rust
#[cfg(feature = "lazy-sweep")]
pub fn pending_sweep_pages() -> usize
```

**Description**: Returns the count of pages currently awaiting lazy sweep.

**Parameters**: None

**Returns**: `usize` - Number of pages that need sweeping

**Behavior**:
1. Iterates through all heap pages
2. Counts pages with `PAGE_FLAG_NEEDS_SWEEP` flag set
3. Returns total count

**Preconditions**:
- GC runtime must be initialized
- Feature flag `lazy-sweep` must be enabled

**Postconditions**:
- Returns accurate count of pages needing sweep
- No modifications to heap state

**Error Conditions**: None (always returns valid count)

**Thread Safety**: Safe to call from any thread; uses thread-local heap access

**Example Usage**:
```rust
use rudo_gc::pending_sweep_pages;

// Check if sweep work has accumulated
let pending = pending_sweep_pages();
if pending > 100 {
    println!("Warning: {} pages pending sweep", pending);
}
```

---

## Feature Flag Configuration

### Cargo.toml

```toml
[features]
default = ["lazy-sweep", "derive"]
lazy-sweep = []  # When disabled, use eager sweep (for testing)
derive = []
```

**Default**: Enabled (lazy sweep is the default behavior)

**Disable**: Users can disable with `--no-default-features --features derive`

---

## Integration Points

### With check_safepoint()

The lazy sweep feature integrates with the existing safepoint mechanism:

```rust
#[cfg(feature = "lazy-sweep")]
pub fn check_safepoint() {
    // ... existing safepoint checks ...

    // Occasionally do lazy sweep work during safepoint checks
    if crate::gc::should_do_lazy_sweep() {
        crate::gc::sweep_pending(heap, 4);
    }
}
```

### With alloc()

The allocation path attempts lazy sweep before requesting new pages:

```rust
pub fn alloc<T>(&mut self) -> NonNull<u8> {
    // 1. Try TLAB
    // 2. Try Free List
    // 3. Try Lazy Sweep (NEW)
    if let Some(ptr) = self.alloc_from_pending_sweep(class_index) {
        return ptr;
    }
    // 4. Alloc Slow (New Page)
}
```

---

## Behavior Matrix

| Scenario | Feature Enabled | Feature Disabled |
|----------|-----------------|------------------|
| Collection sweep | Lazy (incremental) | Eager (STW) |
| Large objects | Eager | Eager |
| Orphan pages | Eager | Eager |
| API available | Yes | No (functions not exported) |
| Memory reclaimed | On next allocation | Immediately after collection |
