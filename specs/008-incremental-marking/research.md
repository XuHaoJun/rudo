# Research: Incremental Marking for Major GC

**Feature**: 008-incremental-marking  
**Date**: 2026-02-03  
**Status**: Complete

This document captures research findings and design decisions for incremental marking implementation.

---

## 1. Incremental Marking Algorithm Selection

### Decision: Hybrid SATB + Dijkstra Insertion Barrier

**Rationale**: Combines strengths of both approaches for correctness and simplicity.

- **SATB (Snapshot-At-The-Beginning)**: Records old values when pointers are overwritten. Ensures objects reachable at snapshot time are not lost.
- **Dijkstra Insertion Barrier**: Marks new pointer values immediately. Prevents newly-reachable objects from being missed.

**Why Hybrid**:
1. Pure SATB requires processing all recorded old values before completion, which can delay termination
2. Pure Dijkstra can cause "floating garbage" (objects that become garbage during marking are retained)
3. Hybrid gets correctness from SATB while Dijkstra reduces floating garbage

**Alternatives Considered**:

| Algorithm | Pros | Cons | Decision |
|-----------|------|------|----------|
| Pure SATB | Simple barrier | Large overwrite buffer, delayed termination | Rejected |
| Pure Dijkstra | Less floating garbage | Requires re-scan on completion | Rejected |
| Yuasa (deletion barrier) | Proven in JVM | Complex card table interaction | Rejected |
| **Hybrid SATB+Dijkstra** | Correctness + reduced re-scanning | Slightly more barrier work | **Selected** |

**ChezScheme Reference**: ChezScheme uses card-based tracking with generation-pair organization. During marking, `use_marks` flag switches barrier behavior to record new dirty cards. We adapt this pattern using our dirty page list.

---

## 2. Write Barrier Design

### Decision: Fast Path + Per-Thread Remembered Buffer

**Rationale**: Minimize hot-path overhead while maintaining correctness.

```rust
// Fast path: check single atomic flag
if !is_incremental_marking_active() && !is_old_to_young_write(...) {
    return; // No barrier work needed
}

// Slow path: per-thread buffered recording
record_in_remembered_buffer(page);
if buffer_full() {
    flush_to_global_dirty_list();
}
```

**Performance Considerations**:

1. **Single atomic load on fast path**: `is_incremental_marking_active()` checks global phase
2. **Per-thread buffers**: Avoid lock contention on global dirty list
3. **Buffer size**: 32 entries default, flush on overflow
4. **Batch processing**: Dirty pages processed in batches during mark slices

**ChezScheme Reference**: Uses `dirty_bytes[]` per-segment array with byte-per-card granularity. Our page-level granularity is coarser but integrates with existing spec 007 infrastructure.

---

## 3. Dirty Page Integration with Spec 007

### Decision: Reuse Existing Infrastructure with Snapshot Enhancement

**Rationale**: Spec 007 already provides mutex-protected dirty page lists. We extend with snapshot mechanism.

**Integration Points**:

```rust
impl LocalHeap {
    // Existing from spec 007
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    // Enhanced for incremental marking
    pub fn take_dirty_pages_snapshot(&mut self) -> usize;
    pub fn dirty_pages_iter(&self) -> impl Iterator<Item = NonNull<PageHeader>>;
    pub fn clear_dirty_pages_snapshot(&mut self);
}
```

**Snapshot Protocol**:

1. At mark slice start: `take_dirty_pages_snapshot()` moves dirty pages to snapshot
2. During slice: Scan snapshot pages for unmarked references
3. New mutations go to fresh dirty list (not snapshot)
4. At slice end: Check if snapshot drained; if not, continue next slice

**Completion Criteria**:
- Worklist empty AND
- Dirty pages snapshot empty AND
- No new dirty pages (or below threshold)

---

## 4. Work Distribution and Slice Coordination

### Decision: Per-Worker Budgets with Slice Barrier

**Rationale**: Parallel marking requires bounded slices with worker synchronization.

**Coordination Model**:

```rust
pub fn mark_increment(global_budget: usize) -> MarkStatus {
    let per_worker_budget = global_budget / worker_count;
    
    // Each worker marks up to budget
    parallel_mark_slice(per_worker_budget);
    
    // Slice barrier: all workers sync before yielding to mutators
    slice_barrier.wait();
    
    // Check completion
    if worklist_empty() && dirty_snapshot_empty() {
        MarkStatus::Complete
    } else {
        MarkStatus::Pending
    }
}
```

**Budget Parameters**:

| Parameter | Default | Rationale |
|-----------|---------|-----------|
| `increment_size` | 1000 objects | ~5ms slice at 200ns/object |
| `max_dirty_pages` | 1000 pages | 4MB worth of dirty pages |
| `remembered_buffer_len` | 32 entries | Per-thread, flush on overflow |
| `slice_timeout_ms` | 50 | Fallback if slice takes too long |

**Work Stealing**: Enabled within slice, disabled across slice boundaries to maintain budget guarantees. The `worklist.rs` module must respect slice boundaries by checking a `stealing_allowed` flag before allowing cross-thread work stealing. This prevents slice drift where workers steal work from future slices, violating per-worker budget constraints.

---

## 5. New Allocation Handling

### Decision: Mark Black on Allocation

**Rationale**: Objects allocated during incremental marking must not be collected.

**Implementation**:

```rust
pub fn allocate<T: Trace>(&mut self, value: T) -> Gc<T> {
    let ptr = self.alloc_raw::<T>();
    
    // During incremental marking: mark new objects immediately
    if is_incremental_marking_active() {
        set_mark_bit(ptr);  // Object is "black" - won't be collected
    }
    
    Gc::from_raw(ptr)
}
```

**Correctness Argument**:
- Objects allocated during marking are reachable (held by mutator)
- Marking them black prevents premature collection
- They will be properly evaluated in the next GC cycle

---

## 6. Fallback to Stop-The-World

### Decision: Graceful Degradation on Threshold Exceeded

**Rationale**: High mutation rates can cause unbounded dirty page growth. Fallback ensures termination.

**Fallback Triggers**:

| Condition | Threshold | Action |
|-----------|-----------|--------|
| Dirty pages exceed limit | `max_dirty_pages` (1000) | Complete marking STW |
| Slice timeout | `slice_timeout_ms` (50) | Complete marking STW |
| Worklist grows unboundedly | 10x initial size | Complete marking STW |

**Fallback Protocol**:

```rust
if dirty_snapshot.len() > max_dirty_pages || slice_exceeded_timeout() {
    // Switch to STW completion
    stop_all_mutators();
    process_remaining_worklist();
    sweep();
    resume_mutators();
}
```

---

## 7. State Machine Design

### Decision: 5-State Machine with Atomic Transitions

**Rationale**: Clear state model simplifies reasoning about concurrent behavior.

```
IDLE → SNAPSHOT → MARKING → FINAL_MARK → SWEEPING → IDLE
         ↑___________|__________|
              (fallback to STW)
```

**State Definitions**:

| State | Description | Mutator Running | Write Barrier Active |
|-------|-------------|-----------------|---------------------|
| `IDLE` | No collection in progress | Yes | Generational only |
| `SNAPSHOT` | Capturing roots (STW) | No | N/A |
| `MARKING` | Incremental marking | Yes | SATB + Dijkstra |
| `FINAL_MARK` | Final dirty page scan (STW) | No | N/A |
| `SWEEPING` | Reclaiming dead objects | Yes | Generational only |

---

## 8. Minor GC Coordination

### Decision: Block Minor GC During Incremental Major Marking

**Rationale**: Avoid complexity of nested collections.

**Protocol**:

```rust
fn maybe_minor_gc() {
    if is_incremental_marking_active() {
        // Defer minor GC until major marking completes
        return;
    }
    // ... normal minor GC
}
```

**Trade-off**: May increase memory pressure during long incremental marks. Acceptable because:
1. Major GC happens less frequently than minor GC
2. Incremental marks complete faster than STW marks (mutator continues)
3. Complexity of concurrent minor+major is high

---

## 9. Open Questions Resolved

| Question | Resolution |
|----------|------------|
| Budget size for best pause/throughput? | 1000 objects (~5ms slices) |
| Remembered buffer size? | 32 entries per thread |
| Dirty gen tracking granularity? | Per-page (matches existing infra) |
| Fallback thresholds? | 1000 dirty pages, 50ms timeout |
| Idle-time collection? | Deferred to future enhancement |
| Parallel incremental? | Supported via per-worker budgets |
| Default on or opt-in? | Opt-in initially via `IncrementalConfig` |

---

## 10. References

- **ChezScheme GC** (`gc.c`, `alloc.c`): Card-based dirty tracking, `use_marks` flag for barrier mode switching
- **V8 GC**: Idle-time incremental marking (future enhancement)
- **Go GC**: Concurrent marking with write barriers
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"
- **Yuasa**: "Real-time garbage collection on general-purpose machines"
- **Spec 007**: Dirty page list generational GC (prerequisite)

---

*Generated by /speckit.plan | 2026-02-03*
