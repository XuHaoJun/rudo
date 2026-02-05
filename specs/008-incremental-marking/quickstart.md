# Quickstart: Incremental Marking

**Feature**: 008-incremental-marking  
**Date**: 2026-02-03

This guide shows how to enable and use incremental marking in your application.

---

## 1. Enabling Incremental Marking

Incremental marking is **opt-in** by default. Enable it at application startup:

```rust
use rudo_gc::{Gc, IncrementalConfig};

fn main() {
    // Enable incremental marking before any GC activity
    rudo_gc::set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });
    
    // Your application code...
    run_application();
}
```

### Configuration Options

| Option | Default | Description |
|--------|---------|-------------|
| `enabled` | `false` | Enable incremental marking |
| `increment_size` | `1000` | Objects per mark slice |
| `max_dirty_pages` | `1000` | Fallback threshold |
| `remembered_buffer_len` | `32` | Per-thread buffer size |
| `slice_timeout_ms` | `50` | Max slice duration |

---

## 2. Cooperative Scheduling

For best results, call `Gc::yield_now()` in long-running computations:

```rust
use rudo_gc::Gc;

fn process_large_dataset(items: &[Item]) {
    for (i, item) in items.iter().enumerate() {
        process_item(item);
        
        // Yield every 1000 items to allow GC marking
        if i % 1000 == 0 {
            Gc::<()>::yield_now();
        }
    }
}
```

**Note**: `yield_now()` is a no-op when incremental marking is not active, so it's safe to call unconditionally.

---

## 3. Async Applications

For async applications using Tokio, the existing `gc::main` macro handles GC coordination:

```rust
use rudo_gc::{Gc, IncrementalConfig};

#[rudo_gc::main]
async fn main() {
    // Enable incremental marking
    rudo_gc::set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });
    
    // Async tasks will automatically yield at GC checkpoints
    let data = Gc::new(Data { value: 42 });
    
    tokio::spawn(async move {
        // GC-safe async work
        process(data).await;
    });
}
```

---

## 4. Monitoring

Check if incremental marking is in progress:

```rust
if rudo_gc::is_incremental_marking_active() {
    println!("Incremental marking in progress");
}
```

After collection, check if fallback occurred:

```rust
use rudo_gc::MarkStats;

let stats = rudo_gc::last_collection_stats();
if stats.fallback_occurred {
    eprintln!("Warning: Incremental marking fell back to STW");
    eprintln!("Reason: {:?}", stats.fallback_reason);
}
```

---

## 5. Tuning for Your Workload

### Low-Latency Applications

Minimize pause times with smaller slices:

```rust
rudo_gc::set_incremental_config(IncrementalConfig {
    enabled: true,
    increment_size: 500,       // Smaller slices
    slice_timeout_ms: 10,      // Shorter timeout
    ..Default::default()
});
```

### High-Throughput Applications

Allow larger slices for better throughput:

```rust
rudo_gc::set_incremental_config(IncrementalConfig {
    enabled: true,
    increment_size: 5000,      // Larger slices
    max_dirty_pages: 2000,     // Higher threshold before fallback
    ..Default::default()
});
```

### Memory-Constrained Applications

Reduce buffer sizes:

```rust
rudo_gc::set_incremental_config(IncrementalConfig {
    enabled: true,
    remembered_buffer_len: 16,  // Smaller per-thread buffer
    max_dirty_pages: 500,       // Earlier fallback
    ..Default::default()
});
```

---

## 6. Write Barrier Integration

If you have custom `Trace` implementations, ensure write barriers are called:

```rust
use rudo_gc::{Trace, Visitor, GcCell};

struct MyContainer<T: Trace> {
    data: GcCell<Vec<T>>,
}

impl<T: Trace> MyContainer<T> {
    fn set(&self, index: usize, value: T) {
        // GcCell::borrow_mut() automatically triggers the write barrier
        self.data.borrow_mut()[index] = value;
    }
}

// The derive macro handles this automatically for most cases
#[derive(Trace)]
struct AutoContainer<T: Trace> {
    #[gc]
    items: GcCell<Vec<Gc<T>>>,
}
```

---

## 7. Troubleshooting

### Frequent Fallbacks to STW

If you see frequent fallbacks:

1. **High mutation rate**: Your code modifies many references during marking
   - Solution: Increase `max_dirty_pages` or reduce mutation frequency
   
2. **Large reference graphs**: Many objects to mark per slice
   - Solution: Increase `increment_size` or call `yield_now()` more often

### Long Pause Times Despite Incremental

1. **Final mark phase taking too long**: Many dirty pages accumulated
   - Solution: Lower `max_dirty_pages` for earlier fallback
   
2. **Not yielding**: Long computations without `yield_now()`
   - Solution: Add yield points in hot loops

### Memory Growth During Marking

1. **New allocations marked black**: Expected behavior during incremental marking
   - These objects survive this cycle but will be collected in the next
   - Solution: This is normal; consider smaller heap or more frequent minor GCs

---

## 8. Comparison: STW vs Incremental

| Aspect | Stop-The-World | Incremental |
|--------|----------------|-------------|
| Max Pause | O(heap_size) | O(increment_size) |
| Total GC Time | Lower | Up to 2x higher |
| Complexity | Simple | Write barrier overhead |
| Best For | Batch processing | Interactive apps |

---

## Next Steps

- Read the [Implementation Plan](./plan.md) for architecture details
- See [Data Model](./data-model.md) for state machine details
- Check [API Contract](./contracts/incremental-api.md) for full API reference

---

*Generated by /speckit.plan | 2026-02-03*
