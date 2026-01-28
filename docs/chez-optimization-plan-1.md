# Chez Scheme Optimization Plan for rudo-gc

## Overview

This document outlines optimization opportunities for rudo-gc based on patterns observed in the Chez Scheme garbage collector implementation. Chez Scheme is a high-performance Scheme compiler with a mature, production-quality GC that has been refined over decades.

## Background

After code review of the parallel marking implementation and analysis of Chez Scheme's GC architecture at `/home/noah/Desktop/rudo/learn-projects/ChezScheme/c/`, several high-value patterns were identified for adoption.

## Target Architecture

### Current State

- Chase-Lev work-stealing deque implemented in `worklist.rs`
- Parallel marking coordinator in `marker.rs`
- Sequential steal loop with potential contention issues
- In-place marking with forwarding pointers

### Desired State

- Reduced contention during work stealing
- Better load distribution via segment ownership
- Lock-free operations with minimal coordination
- Mark bitmap for in-place marking without pointer overhead

---

## Optimization 1: Push-Based Work Transfer

**Priority**: High | **Effort**: Low | **Impact**: Medium

### Problem

Current steal loop in `marker.rs:470` iterates through all queues sequentially:

```rust
for other in all_queues { ... }
```

This causes contention when multiple workers simultaneously attempt to steal from the same victim.

### Chez Scheme Pattern (gc.c:1815-1871)

Push-based remote sweep protocol:
- Worker finding remote reference pushes to `send_remote_sweep_stack`
- Owner reads from `receive_remote_sweep_stack`
- Condition variables wake sleeping workers

### Implementation

Add work notification channel to `PerThreadMarkQueue`:

```rust
struct PerThreadMarkQueue {
    queue: StealQueue<MarkWork>,
    work_available: Arc<NotifyWorker>,
    pending_work: Mutex<Vec<MarkWork>>,
}

impl PerThreadMarkQueue {
    fn push_remote(&self, work: MarkWork) {
        let mut pending = self.pending_work.lock();
        pending.push(work);
        self.work_available.notify();
    }

    fn receive_work(&self) -> Vec<MarkWork> {
        let mut pending = self.pending_work.lock();
        std::mem::take(&mut *pending)
    }
}
```

### Migration Path

1. Add `pending_work: Mutex<Vec<MarkWork>>` to `PerThreadMarkQueue`
2. Add `work_available: Arc<NotifyWorker>` notification mechanism
3. Modify `try_steal_work` to first check `pending_work`, then attempt steal
4. Workers call `receive_work()` when their local queue is empty

---

## Optimization 2: Segment Ownership for Load Distribution

**Priority**: High | **Effort**: Low | **Impact**: Medium

### Problem

Current work stealing treats all pages equally regardless of which thread allocated them. This leads to:
- False sharing when workers mark pages owned by other threads
- Poor cache locality for the owning thread's data

### Chez Scheme Pattern (types.h:154-155, gc.c:138-157)

Each segment tracks its creator thread:

```c
struct seginfo {
    struct thread_gc *creator;  // owning thread
    // ...
};
```

### Implementation

Integrate `owner_thread` from `PageHeader` into work distribution:

```rust
struct PageHeader {
    owner_thread: ThreadId,
    // existing fields...
}

impl GlobalMarkState {
    fn get_owned_queues(&self, thread_id: ThreadId) -> Vec<&PerThreadMarkQueue> {
        self.queues.iter()
            .filter(|q| q.owned_pages.contains_key(thread_id))
            .collect()
    }
}
```

### Migration Path

1. Complete integration of `owner_thread` field in `PageHeader`
2. Add `owned_pages: HashSet<PagePtr>` to `PerThreadMarkQueue`
3. When marking owned pages, push directly to local queue
4. When encountering remote page, push to owner's `pending_work`
5. Use `get_owned_queues()` to prioritize stealing from page owners' queues

---

## Optimization 3: Mark Bitmap

**Priority**: Medium | **Effort**: Medium | **Impact**: High

### Problem

Current in-place marking uses forwarding pointer word:

```rust
// Forwarding pointer occupies first word of object
struct GcBox<T: Trace> {
    forwarding: GcHeader,
    data: T,
}
```

This adds overhead per object and complicates parallel marking.

### Chez Scheme Pattern (gc.c:73-105, types.h:168-169)

Bitmap-based marking with one bit per pointer-sized unit:

```c
typedef struct seginfo {
    octet *marked_mask;        // Bitmap of live objects
    uptr marked_count;         // Number of marked bytes
    // ...
} seginfo;
```

### Implementation

Add page-level bitmap:

```rust
struct PageBitmap {
    bitmap: Vec<u64>,  // One bit per pointer-sized unit
    capacity: usize,   // Number of slots in page
}

impl PageBitmap {
    fn new(capacity: usize) -> Self {
        let bits = (capacity + 63) / 64;
        Self {
            bitmap: vec![0u64; bits],
            capacity,
        }
    }

    fn mark(&mut self, slot_index: usize) {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        self.bitmap[word] |= 1 << bit;
    }

    fn is_marked(&self, slot_index: usize) -> bool {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        (self.bitmap[word] >> bit) & 1 != 0
    }
}
```

### Migration Path

1. Add `PageBitmap` struct
2. Add `bitmap: Option<PageBitmap>` to `PageHeader`
3. Remove forwarding pointer from `GcBox` when using bitmap mode
4. Update mark phase to set bits in bitmap
5. Update sweep phase to read bitmap for liveness
6. Provide backward compatibility mode for forwarding pointer use

---

## Optimization 4: Lock Ordering Enforcement

**Priority**: High | **Effort**: Low | **Impact**: High

### Problem

Current code in `gc.rs:430-435` fixes a deadlock but lacks systematic lock ordering discipline. Future changes may reintroduce issues.

### Chez Scheme Pattern (types.h:402-433)

```c
#define tc_mutex_acquire()     // thread context first
#define alloc_mutex_acquire()  // allocation second (ordered after tc)
```

### Implementation

Define lock ordering in code and documentation:

```rust
// LOCK ORDERING DISCIPLINE
// Order: LocalHeap -> GlobalMarkState -> GC Request
//
// Never acquire LocalHeap while holding GlobalMarkState
// Never acquire GlobalMarkState while holding GC Request
```

### Migration Path

1. Document lock ordering in `sync.rs` or new `locking.md`
2. Add lock order checks in debug builds
3. Use `std::sync::atomic::AtomicU8` for lock tags with ordering assertions
4. Add runtime deadlock detection in tests

---

## Optimization 5: Dynamic Stack Growth

**Priority**: Medium | **Effort**: Low | **Impact**: Low-Medium

### Problem

Current `Vec<MarkWork>` in `PerThreadMarkQueue` grows dynamically but can cause stutter under load.

### Chez Scheme Pattern (gc.c:335-342)

Pre-allocated stack with growth notification:

```c
#define push_sweep(p) do {
    if (tgc->sweep_stack == tgc->sweep_stack_limit)
        enlarge_stack(tgc, &tgc->sweep_stack, ...);
    // ... push ...
} while (0)
```

### Implementation

Add stack monitoring:

```rust
struct PerThreadMarkQueue {
    queue: StealQueue<MarkWork>,
    work_available: Arc<NotifyWorker>,
    capacity_hint: AtomicUsize,  // Target capacity
}

impl PerThreadMarkQueue {
    fn mark(&self, obj: Gc<dyn Trace>) {
        if let Some(work) = self.queue.push_lifo(obj) {
            // Successfully pushed locally
            self.work_available.notify_one();
        } else {
            // Queue full, consider growing or stealing
            self.handle_overflow();
        }
    }
}
```

### Migration Path

1. Add capacity monitoring to `PerThreadMarkQueue`
2. Implement overflow handler that:
   - Pre-allocates additional queue slots
   - Or pushes to remote `pending_work`
3. Add metrics for queue capacity utilization

---

## Implementation Order

### Phase 1: Immediate Improvements (1-2 days)

1. **Lock Ordering Documentation** - Prevents future bugs
2. **Push-Based Work Transfer** - Reduces steal contention
3. **Segment Ownership Integration** - Better load distribution

### Phase 2: Structural Changes (1 week)

4. **Mark Bitmap** - Enables true parallel marking
5. **Dynamic Stack Growth** - Better throughput under load

### Phase 3: Future Considerations

6. Card-based dirty tracking for minor GC optimization
7. Hybrid copy/mark for reduced fragmentation
8. Ephemeron support for weak references

---

## References

- Chez Scheme GC: `/home/noah/Desktop/rudo/learn-projects/ChezScheme/c/gc.c`
- Chez Scheme Types: `/home/noah/Desktop/rudo/learn-projects/ChezScheme/c/types.h`
- Code Review: `/home/noah/Desktop/rudo/docs/review-2.md`
- Parallel Marking Spec: `/home/noah/Desktop/rudo/docs/parallel-marking-spec-1.md`

## Open Questions

1. Should mark bitmap replace forwarding pointers entirely or coexist?
2. What size should the push-based work queue buffer be?
3. How to handle heterogeneous page sizes with ownership?
