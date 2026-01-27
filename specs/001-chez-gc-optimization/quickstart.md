# Quickstart: Chez Scheme GC optimizations for rudo-gc

## Overview

This feature implements five optimizations inspired by the Chez Scheme garbage collector to improve rudo-gc performance in multi-threaded applications.

## Optimizations Implemented

### 1. Push-Based Work Transfer

Reduces contention during work stealing by having workers notify others of available work instead of polling.

```rust
// Before: Polling-based stealing
for other in all_queues {
    if let Some(work) = other.try_steal() {
        return Some(work);
    }
}

// After: Push-based transfer with notification
fn push_remote(&self, work: MarkWork) {
    let mut pending = self.pending_work.lock();
    pending.push(work);
    self.work_available.notify();
}
```

**Files modified**: `src/heap/mark/queue.rs`

---

### 2. Segment Ownership for Load Distribution

Prioritizes marking pages owned by the current thread to improve cache locality.

```rust
// When encountering a remote reference, push to owner's queue
fn mark_object(obj: Gc<dyn Trace>) {
    let page = PageHeader::from_ptr(obj.as_ptr());
    if page.owner_thread != current_thread() {
        let owner_queue = global_state.get_queue(page.owner_thread);
        owner_queue.push_remote(work);
    }
}
```

**Files modified**: `src/heap/mark/ownership.rs`, `src/heap/page.rs`

---

### 3. Mark Bitmap

Replaces per-object forwarding pointers with page-level bitmap (1 bit per pointer slot).

```rust
struct MarkBitmap {
    bitmap: Vec<u64>,  // One bit per pointer-sized unit
    capacity: usize,
    marked_count: AtomicUsize,
}

// Memory comparison:
// - Forwarding pointers: 8 bytes per object
// - Mark bitmap: 1 bit per pointer slot
// For 4KB page with 512 objects:
// - Forwarding: 4096 bytes
// - Bitmap: 64 bytes (98% reduction)
```

**Files added**: `src/heap/mark/bitmap.rs`
**Files modified**: `src/heap/page.rs`, `src/gc.rs`

---

### 4. Lock Ordering Enforcement

Documents and enforces lock acquisition order to prevent deadlocks.

```rust
// LOCK ORDERING DISCIPLINE
// Order: LocalHeap -> GlobalMarkState -> GC Request
//
// Never acquire LocalHeap while holding GlobalMarkState
// Never acquire GlobalMarkState while holding GC Request

// Debug build validation
#[cfg(debug_assertions)]
fn acquire_lock(tag: LockTag) {
    let current_order = tag as u8;
    let min_expected = thread_local::get_min_lock_order();
    assert!(current_order >= min_expected, "Lock ordering violation");
}
```

**Files modified**: `src/heap/sync.rs`, `src/gc.rs`

---

### 5. Dynamic Stack Growth

Monitors queue capacity and grows proactively to prevent stalls.

```rust
struct PerThreadMarkQueue {
    queue: StealQueue<MarkWork>,
    capacity_hint: AtomicUsize,  // Target capacity
}

impl PerThreadMarkQueue {
    fn mark(&self, obj: Gc<dyn Trace>) {
        if let Some(work) = self.queue.push_lifo(obj) {
            self.work_available.notify_one();
        } else if self.queue.len() > self.capacity_hint.load() {
            self.handle_overflow();  // Pre-allocate or push remote
        }
    }
}
```

**Files modified**: `src/heap/mark/queue.rs`

---

## Building and Testing

### Build

```bash
cargo build --workspace
cargo build --release --workspace
```

### Lint

```bash
./clippy.sh
cargo fmt --all
```

### Test

```bash
./test.sh
```

### Miri (for unsafe code)

```bash
./miri-test.sh
```

---

## Performance Targets

| Metric | Baseline | Target | Improvement |
|--------|----------|--------|-------------|
| p95 GC pause time | 100ms | 70ms | 30% reduction |
| Per-object overhead (small objects) | 8 bytes | 4 bytes | 50% reduction |
| Work steal retry rate | 20% | 10% | 50% reduction |
| Deadlock incidents | Occasional | 0 | Prevention |

---

## Migration from Forwarding Pointers

The mark bitmap replaces forwarding pointers in a one-time migration:

1. All existing GcBox allocations are marked using the bitmap
2. Forwarding pointer field removed from GcBox struct
3. Sweep phase updated to read bitmap for liveness
4. Tests verify no objects are incorrectly collected

```rust
// Migration test
#[test]
fn test_forwarding_to_bitmap_migration() {
    let heap = LocalHeap::new();
    let objects: Vec<Gc<()>> = (0..1000).map(|_| heap.alloc(())).collect();

    // Trigger migration
    heap.begin_collection();

    // Verify all objects are marked
    for obj in &objects {
        assert!(bitmap.is_marked(obj.slot_index()));
    }
}
```

---

## Validation

Run the full test suite to verify correctness:

```bash
# All tests including integration
./test.sh --include-ignored

# Multi-threaded marking tests
cargo test --test parallel_marking

# Benchmark for performance validation
cargo bench --marking
```

---

## References

- Chez Scheme GC: `/learn-projects/ChezScheme/c/gc.c`
- Chez Scheme Types: `/learn-projects/ChezScheme/c/types.h`
- Parallel Marking Spec: `/docs/parallel-marking-spec-1.md`
- Optimization Plan: `/docs/chez-optimization-plan-1.md`
