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

**Files added**: `crates/rudo-gc/src/gc/mark/bitmap.rs`
**Files modified**: `crates/rudo-gc/src/heap.rs`, `crates/rudo-gc/src/gc/gc.rs`

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

**Files modified**: `crates/rudo-gc/src/gc/sync.rs`, `crates/rudo-gc/src/heap.rs`, `crates/rudo-gc/src/gc/gc.rs`

---

### 5. Dynamic Stack Growth

Monitors queue capacity and grows proactively to prevent stalls.

```rust
struct PerThreadMarkQueue {
    queue: StealQueue<MarkWork>,
    pending_work: Mutex<Vec<*const GcBox<()>>>,  // Push-based transfer
    capacity_hint: AtomicUsize,  // Target capacity
}

impl PerThreadMarkQueue {
    fn mark(&self, obj: *const GcBox<()>) {
        if let Some(work) = self.queue.push_lifo(obj) {
            // Work pushed locally
        } else if self.queue.len() > self.capacity_hint.load() {
            self.handle_overflow();  // Push to remote or pre-allocate
        }
    }
}
```

**Files modified**: `crates/rudo-gc/src/gc/marker.rs`

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

## Mark Bitmap (Already Implemented)

The mark bitmap is already implemented in the codebase:

1. `PageHeader` already contains `mark_bitmap: [AtomicU64; BITMAP_SIZE]`
2. `GcBox` does not use forwarding pointers (uses ref counting instead)
3. Mark phase uses `PageHeader::set_mark()` to set bits in the bitmap
4. Sweep phase uses `PageHeader::is_marked()` to check liveness

The `MarkBitmap` struct in `gc/mark/bitmap.rs` provides an alternative bitmap implementation
for use cases that need a standalone bitmap (e.g., testing or custom allocation).

**Memory comparison**:
- Per-object marking (if used): 8 bytes per object
- Page-level bitmap: 1 bit per pointer slot
- For 4KB page with 512 objects: 64 bytes vs 4096 bytes (98% reduction)

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
