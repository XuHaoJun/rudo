# Parallel Marking Technical Specification

## 1. Overview

This document specifies the parallel marking implementation for rudo-gc, a mark-sweep garbage collector with generational support. The design is inspired by Chez Scheme's parallel GC architecture while being tailored to rudo-gc's existing thread-local heap structure.

### 1.1 Design Philosophy

1. **Let concurrency emerge naturally** - Work division follows data ownership (page allocation thread)
2. **Minimize synchronization** - Only sync when necessary, use lightest primitives
3. **Respect existing architecture** - Thread-local heaps are natural work division units

### 1.2 Goals

- Parallel marking for both Minor and Major GC
- Work-stealing for load balancing
- Lock-free common paths
- Scalability to multi-core systems

---

## 2. Architecture

### 2.1 High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        Parallel Marking Architecture                         │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   GC Coordinator (collector thread)                                         │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │ 1. request_gc_handshake()     - Stop all threads                    │   │
│   │ 2. take_stack_roots()         - Collect all stack roots             │   │
│   │ 3. ParallelMarkCoordinator    - Initialize workers                   │   │
│   │ 4. distribute_roots()         - Assign roots to work queues          │   │
│   │ 5. distribute_dirty_pages()   - Assign dirty pages (Minor GC)        │   │
│   │ 6. coordinator.mark()         - Execute parallel marking             │   │
│   │ 7. resume_all_threads()       - Resume execution                     │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                      │                                       │
│   ┌────────────────┐ ┌────────────────┐ ┌────────────────┐ ┌────────────────┐│
│   │ PerThreadMark 0│ │ PerThreadMark 1│ │ PerThreadMark 2│ │ PerThreadMark N││
│   │ ┌────────────┐ │ │  ┌────────────┐ │ │  ┌────────────┐ │ │  ┌────────────┐││
│   │ │LocalQueue 0│ │ │  │LocalQueue 1│ │ │  │LocalQueue 2│ │ │  │LocalQueue N│││
│   │ │ (LIFO push)│ │ │  │ (LIFO push)│ │ │  │ (LIFO push)│ │ │  │ (LIFO push)│││
│   │ └──────┬─────┘ │ │  └──────┬─────┘ │ │  └──────┬─────┘ │ │  └──────┬─────┘││
│   └─────────┼───────┘ └─────────┼───────┘ └─────────┼───────┘ └─────────┼───────┘│
│             │                   │                   │                   │         │
│             └───────────────────┼───────────────────┼───────────────────┘         │
│                                 │                   │                            │
│                          ┌──────┴──────┐            │                            │
│                          │             │            │                            │
│                   WorkStealingDeque    │            │                            │
│                   (for all workers)    │            │                            │
│                          │             │            │                            │
│                          ▼             ▼            ▼                            │
│                   ┌─────────────────────────────────────────────────────────┐   │
│                   │              Page Ownership Map                          │   │
│                   │  page_addr -> (owner_thread_id, marked_count, total)    │   │
│                   └─────────────────────────────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Key Components

| Component | Description |
|-----------|-------------|
| `ParallelMarkCoordinator` | Orchestrates parallel marking, manages workers |
| `PerThreadMarkQueue` | Per-thread work queue with local + steal queues |
| `WorkStealingDeque` | Lock-free Chase-Lev style deque |
| `MarkWorker` | Worker thread that processes work items |
| `GcVisitorConcurrent` | Visitor that adds discovered references to work queues |

---

## 3. Data Structures

### 3.1 StealQueue<T, const N: usize>

Lock-free work-stealing queue based on the Chase-Lev algorithm.

```rust
/// Lock-free work stealing queue
/// Based on: "Simple and Efficient Work-Stealing Queues for Parallel Programming"
pub struct StealQueue<T, const N: usize> {
    /// Ring buffer
    buffer: [std::mem::MaybeUninit<T>; N],
    /// Producer pointer
    bottom: Cell<usize>,
    /// Consumer pointer (atomic, for stealing)
    top: AtomicUsize,
    /// Buffer mask (N must be power of 2)
    mask: usize,
}

impl<T, const N: usize> StealQueue<T, N> 
where
    T: Copy,
{
    /// Create new queue (N must be power of 2)
    pub const fn new() -> Self;

    /// Push (LIFO - producer end)
    pub fn push(&self, bottom: &Cell<usize>, item: T) -> bool;

    /// Pop (LIFO - producer end)
    pub fn pop(&self, bottom: &Cell<usize>) -> Option<T>;

    /// Steal (FIFO - consumer end)
    pub fn steal(&self) -> Option<T>;

    /// Get current size
    pub fn len(&self, bottom: &Cell<usize>) -> usize;
}
```

#### Invariants

1. `N` must be a power of 2
2. `mask = N - 1`
3. Size is always `bottom - top` (modulo arithmetic)
4. Queue is empty when `bottom == top`
5. Queue is full when `bottom - top == N`

### 3.2 PerThreadMarkQueue

Per-thread mark queue with local and steal components.

```rust
/// Per-Thread Mark Queue - one per marking thread
pub struct PerThreadMarkQueue {
    /// Local work queue (lock-free push/pop)
    local_queue: Worklist<1024>,
    
    /// Steal queue (can be stolen by other threads)
    steal_queue: StealQueue<NonNull<GcBox<()>>, 1024>,
    
    /// List of pages owned by this thread
    owned_pages: Vec<NonNull<PageHeader>>,
    
    /// Count of marked objects (atomic)
    marked_count: AtomicUsize,
    
    /// Thread ID
    thread_id: std::thread::ThreadId,
}

impl PerThreadMarkQueue {
    /// Push to local queue (LIFO, fastest path)
    pub fn push_local(&mut self, ptr: NonNull<GcBox<()>>);

    /// Pop from local queue
    pub fn pop_local(&mut self) -> Option<NonNull<GcBox<()>>>;

    /// Steal from this queue (called by other threads)
    pub fn steal(&self) -> Option<NonNull<GcBox<()>>>;

    /// Process all objects on an owned page
    pub fn process_owned_page(&mut self, page: NonNull<PageHeader>);

    /// Get marked count
    pub fn marked_count(&self) -> usize;
}
```

### 3.3 ParallelMarkCoordinator

Coordinates parallel marking across multiple workers.

```rust
/// Parallel marking coordinator
pub struct ParallelMarkCoordinator {
    /// Per-thread mark queues
    queues: Vec<PerThreadMarkQueue>,
    
    /// Synchronization barrier
    barrier: Barrier,
    
    /// Page to queue index mapping (page_addr -> queue_idx)
    page_to_queue: HashMap<usize, usize>,
    
    /// Total marked count (atomic)
    total_marked: AtomicUsize,
}

impl ParallelMarkCoordinator {
    /// Create new coordinator
    pub fn new(num_workers: usize) -> Self;

    /// Register pages for a queue
    pub fn register_pages(
        &mut self,
        queue_idx: usize,
        pages: &[NonNull<PageHeader>],
    );

    /// Distribute roots to appropriate queues
    pub fn distribute_roots(
        &self,
        roots: impl Iterator<Item = (&*const u8, &Arc<ThreadControlBlock>)>,
        find_gc_box: impl Fn(*const u8) -> Option<NonNull<GcBox<()>>>,
    );

    /// Distribute dirty pages (Minor GC only)
    pub fn distribute_dirty_pages(&self, heap: &LocalHeap);

    /// Execute parallel marking
    pub fn mark(&self, heap: &LocalHeap, kind: VisitorKind) -> usize;
}
```

### 3.4 PageHeader Modifications

```rust
impl PageHeader {
    /// Atomically try to mark an object (CAS-based)
    /// Returns true if successfully marked (or already marked)
    #[inline]
    pub fn try_mark(&self, index: usize) -> bool {
        let word_idx = index / 64;
        let bit_idx = index % 64;
        let mask = 1u64 << bit_idx;
        
        loop {
            let old = self.mark_bitmap[word_idx].load(Ordering::Acquire);
            
            if old & mask != 0 {
                return false; // Already marked
            }
            
            if self.mark_bitmap[word_idx]
                .compare_exchange_weak(
                    old,
                    old | mask,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
            // CAS failed, retry
        }
    }

    /// Check if all objects in this page are marked
    pub fn is_fully_marked(&self) -> bool {
        let obj_count = self.obj_count as usize;
        for i in 0..((obj_count + 63) / 64) {
            let expected = if i == ((obj_count + 63) / 64) - 1 && obj_count % 64 != 0 {
                (1u64 << (obj_count % 64)) - 1
            } else {
                u64::MAX
            };
            
            if self.mark_bitmap[i].load(Ordering::Acquire) != expected {
                return false;
            }
        }
        true
    }
}
```

---

## 4. Algorithms

### 4.1 Mark Worker Algorithm

```
MARK-WORKER(queue, all_queues, kind):
    marked = 0
    
    // Phase 1: Process owned pages
    for page in queue.owned_pages:
        if kind == MINOR and page.generation != 0:
            continue
        queue.process_owned_page(page)
    
    // Phase 2: Process local queue + steal
    while true:
        // Try local queue (LIFO)
        while (ptr = queue.pop_local()) != None:
            TRACE-AND-MARK(ptr, all_queues)
            marked += 1
        
        // Try stealing (FIFO from other queues)
        if not TRY-STEAL(all_queues, queue):
            break  // No work available
    
    return marked
```

### 4.2 Trace and Mark Algorithm

```
TRACE-AND-MARK(ptr, all_queues):
    // Call trace_fn to discover references
    TRACE-FN(ptr, visitor)
    
where visitor.add_ref(ref) does:
    if ref is a valid GcBox:
        page_addr = ref & PAGE_MASK
        queue_idx = PAGE_TO_QUEUE[page_addr]
        all_queues[queue_idx].push_local(ref)
```

### 4.3 Work Stealing Algorithm

```
TRY-STEAL(all_queues, my_queue):
    for other_queue in all_queues:
        if other_queue == my_queue:
            continue
        
        if (ptr = other_queue.steal()) != None:
            // Find the correct queue for this object
            page_addr = ptr & PAGE_MASK
            owner_idx = FIND-PAGE-OWNER(page_addr, all_queues)
            
            if owner_idx exists:
                all_queues[owner_idx].push_local(ptr)
            else:
                my_queue.push_local(ptr)
            return true
    
    return false
```

### 4.4 Minor GC Dirty Page Processing

For Minor GC, old->young references are tracked via dirty bits:

```
DISTRIBUTE-DIRTY-PAGES(heap, coordinator):
    for page in heap.all_pages:
        if page.generation != 0:  // Skip old gen
            continue
        
        queue_idx = PAGE_TO_QUEUE[page_addr]
        
        for i in 0..page.obj_count:
            if page.is_dirty(i):
                // This is an old->young reference
                gc_box = COMPUTE-GCBOX(page, i)
                coordinator.queues[queue_idx].push_local(gc_box)
        
        page.clear_all_dirty()
```

---

## 5. Configuration

### 5.1 Configuration Parameters

```rust
/// Parallel marking configuration
pub struct ParallelMarkConfig {
    /// Maximum number of parallel marking workers
    /// Default: min(num_cpus, 16)
    pub max_workers: usize,
    
    /// Per-queue capacity
    pub queue_capacity: usize,
    
    /// Enable parallel Minor GC
    pub parallel_minor_gc: bool,
    
    /// Enable parallel Major GC
    pub parallel_major_gc: bool,
}

impl Default for ParallelMarkConfig {
    fn default() -> Self {
        Self {
            max_workers: std::cmp::min(num_cpus::get(), 16),
            queue_capacity: 1024,
            parallel_minor_gc: true,
            parallel_major_gc: true,
        }
    }
}
```

### 5.2 Worker Count Selection

| CPUs | Workers |
|------|---------|
| 1 | 1 |
| 2-4 | min(num_cpus, 4) |
| 5-8 | min(num_cpus, 8) |
| 9+ | min(num_cpus, 16) |

Rationale: Beyond 16 workers, synchronization overhead typically exceeds benefits.

---

## 6. Synchronization

### 6.1 Memory Ordering

| Operation | Ordering | Reason |
|-----------|----------|--------|
| `top.load()` in `steal()` | `Acquire` | Ensure we see complete item before consuming |
| `bottom.store()` in `push()` | `Release` | Make item visible to consumers |
| `mark_bitmap CAS` | `AcqRel` | Atomic mark with proper synchronization |
| `top CAS` in steal | `AcqRel` | Prevent ABA problem |

### 6.2 Synchronization Points

| Point | Mechanism | Purpose |
|-------|-----------|---------|
| Worker start/finish | `Barrier` | Ensure all workers start together |
| Work stealing | Lock-free CAS | No mutex contention on common path |
| Page registration | `Mutex<()>` | One-time setup, low contention |
| GC handshake | Existing `Condvar` | Stop-the-world coordination |

---

## 7. Correctness Guarantees

### 7.1 Invariants

1. **Every reachable object is eventually marked**
   - Roots are added to work queues
   - Tracing adds all referenced objects
   - Work stealing ensures no work is lost

2. **No object is marked twice**
   - `try_mark()` uses CAS to detect already-marked objects
   - Only unmarked objects are added to work queues

3. **All workers eventually terminate**
   - Work queue eventually becomes empty
   - Stealing attempts eventually fail consistently
   - Barrier ensures all workers synchronize

### 7.2 Race Condition Prevention

| Race | Prevention |
|------|------------|
| Two workers mark same object | CAS in `try_mark()` |
| Steal during push | Chase-Lev algorithm guarantees consistency |
| Page ownership conflict | Page assigned to single owner at init |
| GC during marking | Existing `GC_REQUESTED` check |

---

## 8. Files to Modify

| File | Changes |
|------|---------|
| `lib.rs` | Add exports for new modules |
| `gc.rs` | Add parallel marking functions, modify `perform_multi_threaded_collect` |
| `gc/worklist.rs` | **New** - `StealQueue`, `Worklist` |
| `gc/marker.rs` | **New** - `ParallelMarkCoordinator`, `MarkWorker` |
| `heap.rs` | Add page ownership management |
| `heap/page_header.rs` | Add `try_mark()`, `is_fully_marked()` |
| `tests/parallel_gc.rs` | **New** - Comprehensive tests |

---

## 9. Testing Strategy

### 9.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    /// Test: Basic push/pop/steal
    #[test]
    fn test_steal_queue_basic();

    /// Test: FIFO steal order
    #[test]
    fn test_steal_queue_fifo();

    /// Test: LIFO pop order
    #[test]
    fn test_steal_queue_lifo();

    /// Test: Queue empty/full conditions
    #[test]
    fn test_steal_queue_bounds();
}
```

### 9.2 Integration Tests

```rust
#[cfg(test)]
mod parallel_gc_tests {
    /// Test: Multi-threaded Major GC correctness
    #[test]
    fn test_parallel_major_gc();

    /// Test: Multi-threaded Minor GC with dirty pages
    #[test]
    fn test_parallel_minor_gc();

    /// Test: Work stealing load balancing
    #[test]
    fn test_work_stealing_balance();

    /// Test: Cross-thread references
    #[test]
    fn test_cross_thread_references();
}
```

### 9.3 Miri Tests

```rust
#[cfg(test)]
mod miri_tests {
    /// Test: Marking completeness with Miri
    #[test]
    fn test_marking_completeness_miri();
}
```

---

## 10. Performance Considerations

### 10.1 Expected Improvements

```
Marking Time (relative to single-threaded):
┌────────────┬────────────┐
│ # Workers  │ Time Ratio │
├────────────┼────────────┤
│ 1          │ 1.00x      │
│ 2          │ 0.55-0.65x │
│ 4          │ 0.35-0.45x │
│ 8          │ 0.25-0.35x │
│ 16         │ 0.22-0.30x │
└────────────┴────────────┘
```

### 10.2 Potential Bottlenecks

| Bottleneck | Mitigation |
|------------|------------|
| Mark bitmap atomic operations | CAS only on unmarked objects (rare contention) |
| Work stealing synchronization | Lock-free Chase-Lev algorithm |
| trace_fn call overhead | Inlining, batch processing |
| HashMap lookups for page ownership | One-time lookup per object |

---

## 11. Comparison with Chez Scheme

| Aspect | Chez Scheme | rudo-gc |
|--------|-------------|---------|
| Page ownership | `seginfo.creator` | `PageOwnership.owner_thread` |
| Work queue | `sweep_stack` | `PerThreadMarkQueue` |
| Remote work | `send/recv_remote_sweep_stack` | `WorkStealingDeque.steal()` |
| Worker setup | `setup_sweepers()` | `ParallelMarkCoordinator::new()` |
| Worker execution | `run_sweepers()` | `ParallelMarkCoordinator::mark()` |
| Ownership check | `SEGMENT_IS_LOCAL()` | `PAGE_TO_QUEUE` lookup |

---

## 12. References

1. Chase, D., & Lev, Y. (2005). Dynamic Circular Work-Stealing Deques.
2. Dybvig, R. K. (2003). Chez Scheme Version 8 User's Guide.
3. Detlefs, D., et al. (2004). A Comparative Evaluation of Parallel Garbage Collection Implementations.

---

## 13. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-01-27 | rudo-gc team | Initial specification |
