# Research: Parallel Marking for rudo-gc

**Feature**: Parallel Marking for rudo-gc  
**Date**: 2026-01-27  
**Sources**: Chez Scheme implementation, parallel-marking-spec-1.md

---

## Research Questions

| Question | Status | Finding |
|----------|--------|---------|
| How does Chez Scheme implement parallel GC? | Resolved | See Section 1 |
| What work-stealing algorithm to use? | Resolved | See Section 2 |
| How to handle cross-thread references? | Resolved | See Section 3 |
| What synchronization primitives needed? | Resolved | See Section 4 |

---

## 1. Chez Scheme Parallel GC Architecture

### Decision: Use page ownership-based work division

**Rationale**: Chez Scheme assigns each heap segment to the thread that created it (`seginfo.creator`). Work is naturally divided along ownership lines, minimizing synchronization and maximizing cache locality. This approach is well-suited for rudo-gc's existing thread-local heap structure.

**Chez Scheme Implementation**:
```c
#define SEGMENT_IS_LOCAL(si, p) (((si)->creator == tgc) || marked(si, p) || !in_parallel_sweepers)
```

**rudo-gc Adaptation**: Add `owner_thread: std::thread::ThreadId` to `PageHeader`. Use HashMap `page_to_queue` to map page addresses to worker indices.

### Alternatives Considered

| Alternative | Why Rejected |
|-------------|--------------|
| Work stealing without ownership | Higher synchronization overhead, poor locality |
| Centralized work queue | Contention bottleneck, single point of failure |
| Random work distribution | Poor cache locality, unpredictable performance |

---

## 2. Work-Stealing Algorithm

### Decision: Chase-Lev dynamic circular work-stealing deque

**Rationale**: The Chase-Lev algorithm provides:
- O(1) amortized push/pop (LIFO, producer end)
- O(1) amortized steal (FIFO, consumer end)
- Lock-free operations using CAS
- Proven in practice (used in Java ForkJoinPool, Cilk, .NET TPL)

**Algorithm Invariants**:
1. `N` must be a power of 2
2. `mask = N - 1`
3. Size = `bottom - top` (modulo arithmetic)
4. Empty when `bottom == top`
5. Full when `bottom - top == N`

**Memory Ordering**:
| Operation | Ordering | Reason |
|-----------|----------|--------|
| `top.load()` in steal | `Acquire` | Ensure complete item before consuming |
| `bottom.store()` in push | `Release` | Make item visible to consumers |
| `top CAS` in steal | `AcqRel` | Prevent ABA problem |

### Alternatives Considered

| Alternative | Why Rejected |
|-------------|--------------|
| Michael-Scott queue | O(1) all operations but higher constant factors |
| Treiber stack | Simple but no steal operation |
| Lock-based queue | Contention under high load |

---

## 3. Cross-Thread Reference Routing

### Decision: HashMap lookup for page-to-queue mapping

**Rationale**: When a worker discovers a reference to an object in another thread's page, it routes the reference to the owning worker's queue via HashMap lookup. This ensures:
- Each object is processed by its owner's worker
- Cache locality is preserved for owned pages
- Work stealing handles load imbalance

**Routing Algorithm**:
```
when visitor discovers ref:
    page_addr = ref & PAGE_MASK
    queue_idx = PAGE_TO_QUEUE[page_addr]
    all_queues[queue_idx].push_local(ref)
```

### Alternatives Considered

| Alternative | Why Rejected |
|-------------|--------------|
| Send to random queue | Poor locality, higher steal overhead |
| Always process locally | Would require accessing remote pages |
| Central coordinator | Bottleneck and single point of failure |

---

## 4. Synchronization Strategy

### Decision: Barrier + lock-free work stealing

**Rationale**: Use minimal synchronization:
1. **Barrier**: All workers synchronize at start and end of marking phase
2. **Lock-free operations**: Push/pop/steal use CAS, no mutex contention
3. **One-time setup**: Page registration uses `Mutex<()>` (low contention)

**Synchronization Points**:

| Point | Mechanism | Purpose |
|-------|-----------|---------|
| Worker start/finish | `Barrier` | Ensure all workers start together |
| Work stealing | Lock-free CAS | No mutex contention on common path |
| Page registration | `Mutex<()>` | One-time setup, low contention |
| GC handshake | Existing `Condvar` | Stop-the-world coordination |

### Alternatives Considered

| Alternative | Why Rejected |
|-------------|--------------|
| Spin locks | Waste CPU, risk priority inversion |
| Multiple mutexes | Increased complexity, deadlock risk |
| No barrier | Workers could finish at different times |

---

## 5. Implementation Files

### Files to Create

| File | Purpose |
|------|---------|
| `gc/worklist.rs` | `StealQueue<T, N>`, Chase-Lev implementation |
| `gc/marker.rs` | `ParallelMarkCoordinator`, `PerThreadMarkQueue` |
| `tests/parallel_gc.rs` | Comprehensive parallel GC tests |

### Files to Modify

| File | Changes |
|------|---------|
| `heap.rs` | Add `owner_thread`, `try_mark()`, `is_fully_marked()` to `PageHeader` |
| `gc.rs` | Add `parallel_mark_all()`, integrate into `perform_multi_threaded_collect()` |
| `trace.rs` | Add `GcVisitorConcurrent` for parallel marking |

---

## 6. Worker Thread Model

### Decision: Dedicated GC threads (Option A)

**Rationale**:
1. Simpler correctness - mutator threads are paused during marking
2. Matches Chez Scheme's `setup_sweepers()` approach
3. Avoids mutator interference during marking

**Implementation**:
```rust
fn run_marker_worker(
    queue: &PerThreadMarkQueue,
    all_queues: &[Arc<PerThreadMarkQueue>],
    kind: VisitorKind,
) -> usize {
    // Phase 1: Process owned pages
    for page in &queue.owned_pages {
        queue.process_owned_page(page);
    }
    
    // Phase 2: Process local queue + steal
    loop {
        while let Some(ptr) = queue.pop_local() {
            trace_and_mark(ptr, all_queues);
        }
        
        if !try_steal(all_queues, queue) {
            break;
        }
    }
}
```

---

## 7. Testing Strategy

### Unit Tests (in module)

| Test | Purpose |
|------|---------|
| `test_steal_queue_basic` | Verify push/pop/steal operations |
| `test_steal_queue_fifo` | Verify steal order is FIFO |
| `test_steal_queue_lifo` | Verify pop order is LIFO |
| `test_queue_bounds` | Verify empty/full conditions |

### Integration Tests

| Test | Purpose |
|------|---------|
| `test_parallel_major_gc` | Multi-threaded Major GC correctness |
| `test_parallel_minor_gc` | Multi-threaded Minor GC with dirty pages |
| `test_work_stealing_balance` | Load balancing verification |
| `test_cross_thread_references` | Cross-heap reference handling |

### Miri Tests

| Test | Purpose |
|------|---------|
| `test_marking_completeness_miri` | Verify all reachable objects marked |

---

## 8. Performance Targets

| # Workers | Expected Time Ratio | Rationale |
|-----------|---------------------|-----------|
| 1 | 1.00x | Baseline single-threaded |
| 2 | 0.55-0.65x | Near-linear speedup, some overhead |
| 4 | 0.35-0.45x | Good scaling, communication overhead |
| 8 | 0.25-0.35x | Diminishing returns, synchronization overhead |
| 16 | 0.22-0.30x | Plateau, overhead exceeds benefits |

**Rationale**: Beyond 16 workers, synchronization overhead typically exceeds benefits. This aligns with common GC design practices.

---

## 9. Key Differences from Chez Scheme

| Aspect | Chez Scheme | rudo-gc Parallel Marking |
|--------|-------------|-------------------------|
| Work Queue | `sweep_stack` per thread | `PerThreadMarkQueue` |
| Remote Work | `send/recv_remote_sweep_stack` | Work stealing via `StealQueue` |
| Worker Setup | `setup_sweepers()` | `ParallelMarkCoordinator::new()` |
| Execution | `run_sweepers()` | `ParallelMarkCoordinator::mark()` |
| Ownership Check | `SEGMENT_IS_LOCAL()` | Page ownership map |

---

## 10. References

1. Chase, D., & Lev, Y. (2005). Dynamic Circular Work-Stealing Deques.
2. Dybvig, R. K. (2003). Chez Scheme Version 8 User's Guide.
3. Detlefs, D., et al. (2004). A Comparative Evaluation of Parallel Garbage Collection Implementations.
4. rudo-gc existing codebase (heap.rs, gc.rs, trace.rs)
