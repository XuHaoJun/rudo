# rudo-gc Incremental Marking Implementation Plan

**Version**: 0.8.3  
**Date**: 2026-02-03  
**Status**: Draft - Revised with ChezScheme-informed optimizations  
**Dependency**: Requires Generational GC 0.7.x Dirty Page List to be complete  
**Goal**: Reduce major GC pause times through incremental marking integrated with dirty page list tracking

---

## Changelog

### Version 0.8.3 (Revised)

**Key Changes from 0.8.2:**

1. **Parallel Slice Coordination**: Adds explicit per-worker budgets and slice barriers
2. **Write Barrier Fast Path**: Adds remembered-buffer and minimal checks on hot path
3. **Dirty Page Hygiene**: Tracks youngest-generation references to reduce rescan
4. **Snapshot Completion Criteria**: Completion requires worklist empty + dirty snapshot drained
5. **Fallback Thresholds**: Adds clear overflow/timeout fallback to STW completion

### Version 0.8.2 (Revised)

**Key Changes from 0.8.1:**

1. **Dirty Page List Integration**: Updated to work with mutex-protected dirty page lists from spec 007
2. **Snapshot Scanning**: Incremental marking scans dirty page snapshots rather than card tables
3. **Write Barrier Alignment**: Uses existing dirty page list barrier to record old-generation mutations
4. **ChezScheme Alignment**: Keeps the Chez Scheme dirty list + snapshot pattern
5. **Pragmatic Concurrency**: Prioritizes correctness over lock-free buffers in this phase
6. **Compatibility**: Matches current `rudo-gc` implementation of dirty pages in `LocalHeap`

---

## 1. Executive Summary

**Goal**: Implement incremental marking to reduce major collection pause times by splitting the mark phase into smaller, cooperative increments that interleave with mutator execution.

**Target Improvement**: 50-80% reduction in major GC pause times for large heaps by avoiding long stop-the-world marking phases.

**Key Challenge**: Maintaining correctness when objects are modified during the incremental mark phase (requires snapshot-at-the-beginning or incremental update algorithms).

**Timeline**: 3 weeks (after dirty page list generational GC is complete and stable)

---

## 2. Background: Why Incremental Marking?

### 2.1 Current Major Collection Flow

```
Major GC (Current):
1. Stop-the-world (STW)
2. Mark all reachable objects (O(live_objects))
3. Sweep dead objects
4. Resume mutators

Problem: Step 2 can take 100ms+ for large heaps (1GB+)
```

### 2.2 Incremental Marking Flow

```
Major GC (Incremental):
1. STW: Snapshot roots (short)
2. Resume mutators + Mark incrementally (cooperative)
3. STW: Finalize marking (short)
4. Sweep dead objects

Benefit: Mutator runs during most of marking, shorter pauses
```

### 2.3 The Mutator Problem

During incremental marking, mutators can:
- **Create new objects** (white - unmarked)
- **Modify references** (hide objects from marker)
- **Delete references** (expose objects that were hidden)

**Example of Lost Object**:
```
Before mark: A > B > C (C is live)
During mark: Mutator changes A to point to C, drops B (B becomes garbage)
Marker visits A, marks C, never sees B
After mark: B is incorrectly collected (BUG!)
```

**Solution**: Write barrier must record mutations during incremental mark.

---

## 3. Architecture Design

### 3.1 Snapshot-At-The-Beginning (SATB) with Dirty Page List Integration

We use a hybrid SATB + insertion-barrier approach integrated with our dirty page list
generational GC:

1. **Snapshot Phase**: Record all root references at mark start
2. **Incremental Mark**: Process worklist in bounded slices using dirty page snapshots
3. **Write Barrier**: Record overwritten references and immediately mark new values
4. **Slice Coordination**: Split a global budget into per-worker budgets with a slice barrier
5. **Completion Rule**: Marking completes only when worklist is empty and dirty snapshots drain
6. **Fallback**: If snapshots exceed thresholds or slice drift occurs, finish with STW

During MARKING, new allocations are treated as black (mark bit set on allocate) to avoid
missing newly created objects that never enter the worklist.

### 3.2 System Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                    Incremental Marking State Machine          │
├─────────────────────────────────────────────────────────────────────┤
│                                                               │
│  [IDLE] ─────────────────────────────────────────────────────────────────────┐
│                                │                             │
│                                ▼                             │
│                         (STW: short)                         │
│                         Capture roots                        │
│                         Clear marks                          │
│                         Set INCREMENTAL_MARK                 │
│                                │                             │
│                                ▼                             │
│                         [MARKING] ─────────────────────────────────────────────────────────────────────┐
│                         │                        │          │
│                         │ Mark chunks            │          │
│                         │ Check dirty pages      │          │
│                         │                        │          │
│                         ▼                        │          │
│                    Mark complete?                │          │
│                         │                        │          │
│                    Yes ├── No ─────────────────────────────────────────────────────────────────────┐
│                     │                                       │
│                     ▼                                       │
│               (STW: short)                                  │
│               Process final dirty pages                     │
│               Verify marking complete                       │
│                     │                                       │
│                     ▼                                       │
│               [SWEEPING]                                    │
│                     │                                       │
│                     ▼                                       │
│               [IDLE]                                        │
│                                                               │
└──────────────────────────────────────────────────────────────────────┘
```

### 3.3 Data Structures

#### 3.3.1 Incremental State with Dirty Page List Integration

**New File**: `src/gc/incremental.rs`

```rust
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::collections::VecDeque;

/// Global incremental marking state integrated with dirty page list tracking
pub struct IncrementalMarkState {
    /// Current phase of incremental marking
    phase: AtomicUsize,  // 0=IDLE, 1=SNAPSHOT, 2=MARKING, 3=FINAL_MARK, 4=SWEEPING
    
    /// Work queue for objects to mark (lock-free)
    worklist: crossbeam::queue::SegQueue<NonNull<GcBox<()>>>,
    
    /// Dirty page snapshot for incremental marking
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    /// Number of objects marked this increment
    marked_this_increment: AtomicUsize,
    
    /// Target: mark this many objects per increment
    increment_size: usize,
    
    /// Dirty page list (shared with generational GC)
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
}

/// Record of a reference write during incremental marking
/// (Retained for SATB if we need to track overwritten values)
pub struct DirtyRecord {
    /// Old value that was overwritten (needs to be marked)
    old_value: *const GcBox<()>,
}

/// Global singleton
static INCREMENTAL_STATE: OnceLock<IncrementalMarkState> = OnceLock::new();

/// GC phase constants
const PHASE_IDLE: usize = 0;
const PHASE_SNAPSHOT: usize = 1;
const PHASE_MARKING: usize = 2;
const PHASE_FINAL_MARK: usize = 3;
const PHASE_SWEEPING: usize = 4;
```

#### 3.3.2 GC Request Integration

**File**: `src/gc/gc.rs`

Add incremental flag to GC request:

```rust
pub struct GcRequest {
    pub collection_type: CollectionType,
    pub priority: GcPriority,
    pub incremental: bool,  // NEW: Request incremental collection
}

pub enum CollectionType {
    Minor,
    Major,
    IncrementalMajor,  // NEW: Incremental major collection
}
```

#### 3.3.3 Thread-Local Mark Tracking

**File**: `src/heap.rs`

```rust
pub struct ThreadControlBlock {
    // ... existing fields ...
    
    /// Local work queue for incremental marking
    /// Reduces contention on global worklist
    local_mark_queue: Vec<NonNull<GcBox<()>>>,
    
    /// Number of objects this thread has marked this increment
    marked_count: usize,

    /// Per-thread remembered buffer for write barrier batching
    remembered_buffer: Vec<NonNull<PageHeader>>,
}
```

### 3.4 Write Barrier for Incremental Marking with Dirty Page List Integration

#### 3.4.1 Enhanced Write Barrier

**File**: `src/cell.rs` and `src/gc/gc.rs`

The write barrier must:
1. Apply dirty page list generational barrier (old→young tracking)
2. Apply incremental barrier (record overwritten references)
3. Keep a fast path when incremental marking is off or the write is old→old
4. Optionally batch writes in a small remembered buffer to reduce lock contention

```rust
/// Combined write barrier for dirty page list generational + incremental GC
fn write_barrier<T: Trace>(&self, old_value: Option<&T>, new_value: &T) {
    // Fast path: if no incremental marking and not old->young, return early.
    if !is_incremental_marking_active()
        && !(self.is_old_generation() && new_value.is_young_generation())
    {
        return;
    }

    // 1. Dirty page list generational barrier (from spec 007)
    if self.is_old_generation() && new_value.is_young_generation() {
        // Optionally record into a small per-thread remembered buffer,
        // flushing to the global dirty list on overflow.
        add_to_dirty_pages(self.page());
    }

    // 2. Incremental barrier (new for 0.8)
    if is_incremental_marking_active() {
        if let Some(old) = old_value {
            // Record the OLD value that was overwritten.
            record_dirty_reference(old);
        }

        // Also mark the NEW value immediately (Dijkstra-style).
        mark_if_unmarked(new_value);
    }
}
```

#### 3.4.2 Dirty Page Snapshot Processing

**File**: `src/gc/incremental.rs`

```rust
impl IncrementalMarkState {
    /// Called during each marking increment
    pub fn process_dirty_pages(&mut self) {
        // Snapshot dirty pages for lock-free scanning
        let mut dirty_pages = self.dirty_pages.lock();
        self.dirty_pages_snapshot.clear();
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        drop(dirty_pages);
    }
    
    /// Record a dirty page (called from write barrier)
    pub fn record_dirty_page(&self, page: NonNull<PageHeader>) {
        let mut dirty_pages = self.dirty_pages.lock();
        dirty_pages.push(page);
    }
}
```

Implementation notes:
1. Track a per-page or per-snapshot "youngest generation referenced" to avoid rescanning pages
   that only contain old→old references.
2. If the snapshot grows beyond `max_dirty_pages`, fall back to STW completion to preserve
   correctness and avoid unbounded incremental overhead.
3. When a page is scanned and found clean (no young refs), clear its dirty flag immediately.

### 3.5 Incremental Mark Loop with Dirty Page List Integration

#### 3.5.1 Cooperative Yield Points

**File**: `src/gc/gc.rs`

```rust
/// Mark a chunk of objects, then yield.
/// Uses a global budget that is split into per-worker budgets to bound slice time.
pub fn mark_increment(heap: &LocalHeap, global_budget: usize) -> MarkStatus {
    let state = incremental_state();
    let local_budget = split_budget_per_worker(global_budget);
    let mut marked = 0;

    // 1. Process worklist up to budget
    while marked < local_budget {
        if let Some(ptr) = state.worklist.pop() {
            unsafe {
                let gc_box = ptr.as_ptr();
                if !(*gc_box).is_marked() {
                    (*gc_box).set_mark();
                    ((*gc_box).trace_fn)(ptr.as_ptr().cast(), &mut IncrementalVisitor);
                }
            }
            marked += 1;
        } else {
            // Worklist empty - scan dirty pages snapshot
            state.process_dirty_pages();
            if state.dirty_pages_snapshot.is_empty() {
                break;
            }
            for page in state.dirty_pages_snapshot.drain(..) {
                scan_dirty_page(page, &state.worklist);
            }
        }
    }

    // 2. Slice barrier: all workers rendezvous here to enforce slice boundary.
    // Work-stealing is allowed only within the local_budget window to avoid slice drift.
    slice_barrier_wait();

    if is_marking_complete() {
        MarkStatus::Complete
    } else {
        MarkStatus::Yield
    }
}

/// Check if marking is complete.
/// Completion requires both the global worklist and dirty snapshots to be empty.
pub fn is_marking_complete() -> bool {
    let state = incremental_state();
    state.worklist.is_empty() && state.dirty_pages_snapshot.is_empty()
}
```

#### 3.5.2 Mutator Yield Integration

**File**: `src/lib.rs`

Provide a cooperative yield function for long-running computations:

```rust
impl Gc {
    /// Yield to GC during long computations
    /// Call this periodically in loops that allocate heavily
    pub fn yield_now() {
        if is_incremental_marking_active() {
            // Allow GC to run an increment
            incremental_mark_step();
        }
    }
}
```

---

## 4. Implementation Phases

### Phase 1: State Management and Infrastructure (Week 1)

**Tasks**:
1. Create `src/gc/incremental.rs` with state management
2. Add phase constants and atomic state transitions
3. Implement worklist with crossbeam queue
4. Add dirty page snapshot buffer
5. Create `is_incremental_marking_active()` helper
6. Unit tests for state machine

**Deliverables**:
- `src/gc/incremental.rs` - Core incremental state
- `tests/incremental_state.rs` - State machine tests

### Phase 2: Write Barrier Integration (Week 1-2)

**Tasks**:
1. Enhance write barrier to support incremental marking with dirty page list integration
2. Implement dirty page snapshot recording
3. Add write barrier to GcCell, Gc<T>, and derived Trace impls
4. Add a small per-thread remembered buffer to batch dirty page updates
5. Tests for write barrier correctness

**Deliverables**:
- Modified `src/cell.rs` - Incremental write barrier
- Modified `src/gc/gc.rs` - Barrier integration
- Modified `rudo-gc-derive/` - Trace macro updates
- `tests/incremental_write_barrier.rs` - Barrier tests

### Phase 3: Mark Loop and Snapshots (Week 2)

**Tasks**:
1. Implement snapshot-at-beginning root capture
2. Create incremental mark loop with global + per-worker budgets
3. Integrate with existing parallel marking infrastructure (slice barrier)
4. Add dirty page snapshot processing
5. Mark completion detection

**Deliverables**:
- Modified `src/gc/marker.rs` - Incremental worker integration
- Modified `src/gc/gc.rs` - Incremental collection entry points
- `tests/incremental_marking.rs` - Mark loop tests

### Phase 4: Final Mark and Integration (Week 2-3)

**Tasks**:
1. Implement final mark phase (STW to complete marking)
2. Integration with dirty page list generational GC (spec 007)
3. Gc::yield_now() for cooperative scheduling
4. Configuration options (increment_size, enable/disable)
5. Full integration tests

**Deliverables**:
- Modified `src/lib.rs` - Public API and yield
- Modified `src/gc/gc.rs` - Integration
- `tests/incremental_integration.rs` - Full workflow tests
- `tests/incremental_generational.rs` - Combined GC tests

### Phase 5: Testing and Optimization (Week 3)

**Tasks**:
1. Run full test suite
2. Create pause time benchmarks
3. Profile and optimize hot paths
4. Stress tests for concurrent scenarios
5. Documentation

**Deliverables**:
- `benchmarks/incremental_pause.rs` - Pause benchmarks
- Performance comparison report
- Updated documentation

---

## 5. File Changes Summary

### 5.1 New Files (4)

| File | Purpose |
|------|---------|
| `src/gc/incremental.rs` | Core incremental marking state |
| `tests/incremental_state.rs` | State machine tests |
| `tests/incremental_write_barrier.rs` | Barrier tests |
| `tests/incremental_integration.rs` | Full workflow tests |

### 5.2 Modified Files (7)

| File | Changes |
|------|---------|
| `src/gc/gc.rs` | Incremental collection, write barrier integration |
| `src/cell.rs` | Enhanced write barrier |
| `src/heap.rs` | Thread-local mark queues |
| `src/gc/marker.rs` | Incremental worker support |
| `src/gc/worklist.rs` | Work-stealing for incremental |
| `src/lib.rs` | Gc::yield_now(), public API |
| `rudo-gc-derive/` | Trace macro updates |

---

## 6. Testing Strategy

### 6.1 Correctness Tests

```rust
// Example: Test that mutator changes don't lose objects
#[test]
fn test_incremental_no_lost_objects() {
    loom::model(|| {
        // Start incremental marking
        start_incremental_mark();
        
        let obj_a = Gc::new(Data { value: 1 });
        let obj_b = Gc::new(Data { value: 2 });
        let obj_c = Gc::new(Data { value: 3 });
        
        // Mutator modifies references during marking
        let cell = GcCell::new(obj_b);
        *cell.borrow_mut() = obj_c;  // Write barrier records this
        
        // Continue marking
        run_incremental_mark_to_completion();
        
        // All objects must survive
        assert!(!obj_a.is_dead());
        assert!(!obj_c.is_dead());  // obj_b can be collected
    });
}
```

### 6.2 Pause Time Benchmarks

```rust
// Example: Measure incremental vs STW pause times
fn bench_incremental_pause(c: &mut Criterion) {
    c.bench_function("stw_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_stw();
            start.elapsed()
        });
    });
    
    c.bench_function("incremental_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_incremental();
            // Measure longest single pause, not total time
            incremental_max_pause_time()
        });
    });
}
```

### 6.3 Stress Tests

- Concurrent allocation during incremental mark
- Heavy mutation rate during mark phase
- Large object graphs (deep trees, cycles)
- Multi-threaded mutators with shared data
- Parallel slice budget drift under work-stealing
- Remembered buffer overflow/flush ordering
- Dirty page youngest-generation tracking correctness

---

## 7. Risk Assessment

### 7.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Lost objects due to race | Medium | Critical | Extensive loom tests, SATB algorithm |
| Write barrier overhead | High | Medium | Profile hot path, optimize fast path |
| Memory bloat (dirty pages) | Medium | Medium | Bounded buffer, overflow to STW |
| Complexity in GC state | Medium | High | Clear state machine, thorough testing |
| Integration with generational | Medium | Medium | Test combined scenarios |
| Slice boundary drift (parallel marking) | Medium | High | Per-worker budgets + barrier sync |
| Remembered buffer overflow | Medium | Medium | Flush ordering tests, size defaults |
| Dirty youngest-gen tracking | Low | Medium | Verify rescan rules in loom tests |

### 7.2 Mitigation Strategies

1. **Formal Verification**: Use loom for all concurrent scenarios
2. **Bounded Buffers**: Dirty record buffer has max size; overflow triggers STW completion
3. **Gradual Rollout**: Feature flag for testing before default-on
4. **Fallback**: Can always fall back to STW marking if incremental fails

---

## 8. Success Criteria

### 8.1 Functional Requirements

- [ ] No lost objects during incremental marking (loom tests pass)
- [ ] No double-free or UAF (Miri clean)
- [ ] All existing tests pass (./test.sh)
- [ ] Write barrier correct under concurrent mutation
- [ ] Completion check requires worklist empty + dirty snapshot drained
- [ ] Slice budgets respected under parallel marking
- [ ] Graceful fallback to STW on overflow

### 8.2 Performance Requirements

| Metric | Target | Measurement |
|--------|--------|-------------|
| Max pause time (1GB heap) | < 10ms | Benchmarks |
| Total GC time | No more than 2x STW | Comparison benchmark |
| Mutator utilization | > 90% during mark | Instrumentation |
| Write barrier overhead | < 10% vs generational only | Microbenchmarks |

---

## 9. Relationship to Dirty Page List Generational GC

### 9.1 Integration Points

1. **Write Barrier**: Combines dirty page list generational and incremental checks
   ```rust
   fn write_barrier(old, new) {
       dirty_page_barrier(new);   // From spec 007
       incremental_barrier(old);  // New for 0.8
   }
   ```

2. **Collection Scheduling**:
   - Minor GC: STW, uses dirty page list (spec 007)
   - Major GC: Incremental, uses write barrier + dirty page snapshots
   - Incremental slices are bounded by a global budget split per worker
   - All workers rendezvous at the slice barrier before yielding to mutators

3. **Phase Coordination**:
   - Cannot run minor GC during incremental major mark
   - Must complete or abort incremental mark before minor GC
   - Exceeding snapshot or budget thresholds triggers STW completion

### 9.2 Configuration

```rust
pub struct GcConfig {
    // From spec 007
    pub generational: bool,
    pub dirty_page_list: bool,
    
    // New for 0.8
    pub incremental_marking: bool,
    pub increment_size: usize,  // Objects per increment
    pub max_dirty_pages: usize,  // Snapshot size before fallback
    pub remembered_buffer_len: usize, // Per-thread write barrier buffer
    pub track_dirty_youngest_gen: bool, // Skip rescan when only old refs remain
}
```

---

## 10. Comparison with Original 0.8 Plan

| Aspect | Original 0.8 Plan | Revised 0.8.3 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Mutex-protected page lists | Dirty page list snapshots |
| **Write Barrier** | Mutex contention | Fast path + remembered buffer (Chez pattern) |
| **State Management** | Global state machine | Dirty page snapshots |
| **Slice Coordination** | Not specified | Per-worker budgets + slice barrier |
| **Dirty Hygiene** | Rescan on every pass | Track youngest generation, clear on clean |
| **Complexity** | High (Mutex, snapshots, double-checks) | Moderate (reuses existing infra) |
| **Thread Safety** | Mutex + double-check patterns | Mutex + double-check patterns |
| **Performance** | Write barrier regression | Targeted O(dirty_pages) scans |
| **Risk Level** | High | Medium (reuses proven pattern) |

---

## 11. References

- **ChezScheme GC**: Reference for incremental marking implementation
- **V8 GC**: Inspiration for idle-time incremental marking
- **Go GC**: Reference for concurrent marking with write barriers
- **Dirty Page List GC (spec 007)**: Prerequisite for this plan
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"

---

## 12. Open Questions

1. **Budget Size**: What increment_size provides best pause/throughput tradeoff?
2. **Remembered Buffer Size**: What default size balances overhead and contention?
3. **Dirty Gen Tracking**: Should youngest-gen tracking be per-page or per-snapshot?
4. **Fallback Thresholds**: What max_dirty_pages and budget timeout are acceptable?
5. **Idle-Time Collection**: Should we mark during idle time (like V8)?
6. **Parallel Incremental**: How to parallelize marking without losing increments?
7. **Feature Flag**: Should incremental be default-on or opt-in initially?

---

*Document generated: 2026-02-03*  
*Based on: Dybvig Review and ChezScheme Reference*  
*Prerequisite: Dirty Page List GC (spec 007) must be complete and stable*
# rudo-gc Incremental Marking Implementation Plan


### 3.4 Write Barrier for Incremental Marking with Dirty Page List Integration

#### 3.4.1 Enhanced Write Barrier

**File**: `src/cell.rs` and `src/gc/gc.rs`

The write barrier must:
1. Apply dirty page list generational barrier (old→young tracking)
2. Apply incremental barrier (record overwritten references)
3. Keep a fast path when incremental marking is off or the write is old→old
4. Optionally batch writes in a small remembered buffer to reduce lock contention

```rust
/// Combined write barrier for dirty page list generational + incremental GC
fn write_barrier<T: Trace>(&self, old_value: Option<&T>, new_value: &T) {
    // Fast path: if no incremental marking and not old->young, return early.
    if !is_incremental_marking_active()
        && !(self.is_old_generation() && new_value.is_young_generation())
    {
        return;
    }

    // 1. Dirty page list generational barrier (from spec 007)
    if self.is_old_generation() && new_value.is_young_generation() {
        // Optionally record into a small per-thread remembered buffer,
        // flushing to the global dirty list on overflow.
        add_to_dirty_pages(self.page());
    }

    // 2. Incremental barrier (new for 0.8)
    if is_incremental_marking_active() {
        if let Some(old) = old_value {
            // Record the OLD value that was overwritten.
            record_dirty_reference(old);
        }

        // Also mark the NEW value immediately (Dijkstra-style).
        mark_if_unmarked(new_value);
    }
}
```

#### 3.4.2 Dirty Page Snapshot Processing

**File**: `src/gc/incremental.rs`

```rust
impl IncrementalMarkState {
    /// Called during each marking increment
    pub fn process_dirty_pages(&mut self) {
        // Snapshot dirty pages for lock-free scanning
        let mut dirty_pages = self.dirty_pages.lock();
        self.dirty_pages_snapshot.clear();
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        drop(dirty_pages);
    }
    
    /// Record a dirty page (called from write barrier)
    pub fn record_dirty_page(&self, page: NonNull<PageHeader>) {
        let mut dirty_pages = self.dirty_pages.lock();
        dirty_pages.push(page);
    }
}
```

Implementation notes:
1. Track a per-page or per-snapshot "youngest generation referenced" to avoid rescanning pages
   that only contain old→old references.
2. If the snapshot grows beyond `max_dirty_pages`, fall back to STW completion to preserve
   correctness and avoid unbounded incremental overhead.
3. When a page is scanned and found clean (no young refs), clear its dirty flag immediately.

### 3.5 Incremental Mark Loop with Dirty Page List Integration

#### 3.5.1 Cooperative Yield Points

**File**: `src/gc/gc.rs`

```rust
/// Mark a chunk of objects, then yield.
/// Uses a global budget that is split into per-worker budgets to bound slice time.
pub fn mark_increment(heap: &LocalHeap, global_budget: usize) -> MarkStatus {
    let state = incremental_state();
    let local_budget = split_budget_per_worker(global_budget);
    let mut marked = 0;

    // 1. Process worklist up to budget
    while marked < local_budget {
        if let Some(ptr) = state.worklist.pop() {
            unsafe {
                let gc_box = ptr.as_ptr();
                if !(*gc_box).is_marked() {
                    (*gc_box).set_mark();
                    ((*gc_box).trace_fn)(ptr.as_ptr().cast(), &mut IncrementalVisitor);
                }
            }
            marked += 1;
        } else {
            // Worklist empty - scan dirty pages snapshot
            state.process_dirty_pages();
            if state.dirty_pages_snapshot.is_empty() {
                break;
            }
            for page in state.dirty_pages_snapshot.drain(..) {
                scan_dirty_page(page, &state.worklist);
            }
        }
    }

    // 2. Slice barrier: all workers rendezvous here to enforce slice boundary.
    // Work-stealing is allowed only within the local_budget window to avoid slice drift.
    slice_barrier_wait();

    if is_marking_complete() {
        MarkStatus::Complete
    } else {
        MarkStatus::Yield
    }
}

/// Check if marking is complete.
/// Completion requires both the global worklist and dirty snapshots to be empty.
pub fn is_marking_complete() -> bool {
    let state = incremental_state();
    state.worklist.is_empty() && state.dirty_pages_snapshot.is_empty()
}
```

#### 3.5.2 Mutator Yield Integration

**File**: `src/lib.rs`

Provide a cooperative yield function for long-running computations:

```rust
impl Gc {
    /// Yield to GC during long computations
    /// Call this periodically in loops that allocate heavily
    pub fn yield_now() {
        if is_incremental_marking_active() {
            // Allow GC to run an increment
            incremental_mark_step();
        }
    }
}
```

---

## 4. Implementation Phases

### Phase 1: State Management and Infrastructure (Week 1)

**Tasks**:
1. Create `src/gc/incremental.rs` with state management
2. Add phase constants and atomic state transitions
3. Implement worklist with crossbeam queue
4. Add dirty page snapshot buffer
5. Create `is_incremental_marking_active()` helper
6. Unit tests for state machine

**Deliverables**:
- `src/gc/incremental.rs` - Core incremental state
- `tests/incremental_state.rs` - State machine tests

### Phase 2: Write Barrier Integration (Week 1-2)

**Tasks**:
1. Enhance write barrier to support incremental marking with dirty page list integration
2. Implement dirty page snapshot recording
3. Add write barrier to GcCell, Gc<T>, and derived Trace impls
4. Add a small per-thread remembered buffer to batch dirty page updates
5. Tests for write barrier correctness

**Deliverables**:
- Modified `src/cell.rs` - Incremental write barrier
- Modified `src/gc/gc.rs` - Barrier integration
- Modified `rudo-gc-derive/` - Trace macro updates
- `tests/incremental_write_barrier.rs` - Barrier tests

### Phase 3: Mark Loop and Snapshots (Week 2)

**Tasks**:
1. Implement snapshot-at-beginning root capture
2. Create incremental mark loop with global + per-worker budgets
3. Integrate with existing parallel marking infrastructure (slice barrier)
4. Add dirty page snapshot processing
5. Mark completion detection

**Deliverables**:
- Modified `src/gc/marker.rs` - Incremental worker integration
- Modified `src/gc/gc.rs` - Incremental collection entry points
- `tests/incremental_marking.rs` - Mark loop tests

### Phase 4: Final Mark and Integration (Week 2-3)

**Tasks**:
1. Implement final mark phase (STW to complete marking)
2. Integration with dirty page list generational GC (spec 007)
3. Gc::yield_now() for cooperative scheduling
4. Configuration options (increment_size, enable/disable)
5. Full integration tests

**Deliverables**:
- Modified `src/lib.rs` - Public API and yield
- Modified `src/gc/gc.rs` - Integration
- `tests/incremental_integration.rs` - Full workflow tests
- `tests/incremental_generational.rs` - Combined GC tests

### Phase 5: Testing and Optimization (Week 3)

**Tasks**:
1. Run full test suite
2. Create pause time benchmarks
3. Profile and optimize hot paths
4. Stress tests for concurrent scenarios
5. Documentation

**Deliverables**:
- `benchmarks/incremental_pause.rs` - Pause benchmarks
- Performance comparison report
- Updated documentation

---

## 5. File Changes Summary

### 5.1 New Files (4)

| File | Purpose |
|------|---------|
| `src/gc/incremental.rs` | Core incremental marking state |
| `tests/incremental_state.rs` | State machine tests |
| `tests/incremental_write_barrier.rs` | Barrier tests |
| `tests/incremental_integration.rs` | Full workflow tests |

### 5.2 Modified Files (7)

| File | Changes |
|------|---------|
| `src/gc/gc.rs` | Incremental collection, write barrier integration |
| `src/cell.rs` | Enhanced write barrier |
| `src/heap.rs` | Thread-local mark queues |
| `src/gc/marker.rs` | Incremental worker support |
| `src/gc/worklist.rs` | Work-stealing for incremental |
| `src/lib.rs` | Gc::yield_now(), public API |
| `rudo-gc-derive/` | Trace macro updates |

---

## 6. Testing Strategy

### 6.1 Correctness Tests

```rust
// Example: Test that mutator changes don't lose objects
#[test]
fn test_incremental_no_lost_objects() {
    loom::model(|| {
        // Start incremental marking
        start_incremental_mark();
        
        let obj_a = Gc::new(Data { value: 1 });
        let obj_b = Gc::new(Data { value: 2 });
        let obj_c = Gc::new(Data { value: 3 });
        
        // Mutator modifies references during marking
        let cell = GcCell::new(obj_b);
        *cell.borrow_mut() = obj_c;  // Write barrier records this
        
        // Continue marking
        run_incremental_mark_to_completion();
        
        // All objects must survive
        assert!(!obj_a.is_dead());
        assert!(!obj_c.is_dead());  // obj_b can be collected
    });
}
```

### 6.2 Pause Time Benchmarks

```rust
// Example: Measure incremental vs STW pause times
fn bench_incremental_pause(c: &mut Criterion) {
    c.bench_function("stw_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_stw();
            start.elapsed()
        });
    });
    
    c.bench_function("incremental_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_incremental();
            // Measure longest single pause, not total time
            incremental_max_pause_time()
        });
    });
}
```

### 6.3 Stress Tests

- Concurrent allocation during incremental mark
- Heavy mutation rate during mark phase
- Large object graphs (deep trees, cycles)
- Multi-threaded mutators with shared data
- Parallel slice budget drift under work-stealing
- Remembered buffer overflow/flush ordering
- Dirty page youngest-generation tracking correctness

---

## 7. Risk Assessment

### 7.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Lost objects due to race | Medium | Critical | Extensive loom tests, SATB algorithm |
| Write barrier overhead | High | Medium | Profile hot path, optimize fast path |
| Memory bloat (dirty pages) | Medium | Medium | Bounded buffer, overflow to STW |
| Complexity in GC state | Medium | High | Clear state machine, thorough testing |
| Integration with generational | Medium | Medium | Test combined scenarios |
| Slice boundary drift (parallel marking) | Medium | High | Per-worker budgets + barrier sync |
| Remembered buffer overflow | Medium | Medium | Flush ordering tests, size defaults |
| Dirty youngest-gen tracking | Low | Medium | Verify rescan rules in loom tests |

### 7.2 Mitigation Strategies

1. **Formal Verification**: Use loom for all concurrent scenarios
2. **Bounded Buffers**: Dirty record buffer has max size; overflow triggers STW completion
3. **Gradual Rollout**: Feature flag for testing before default-on
4. **Fallback**: Can always fall back to STW marking if incremental fails

---

## 8. Success Criteria

### 8.1 Functional Requirements

- [ ] No lost objects during incremental marking (loom tests pass)
- [ ] No double-free or UAF (Miri clean)
- [ ] All existing tests pass (./test.sh)
- [ ] Write barrier correct under concurrent mutation
- [ ] Completion check requires worklist empty + dirty snapshot drained
- [ ] Slice budgets respected under parallel marking
- [ ] Graceful fallback to STW on overflow

### 8.2 Performance Requirements

| Metric | Target | Measurement |
|--------|--------|-------------|
| Max pause time (1GB heap) | < 10ms | Benchmarks |
| Total GC time | No more than 2x STW | Comparison benchmark |
| Mutator utilization | > 90% during mark | Instrumentation |
| Write barrier overhead | < 10% vs generational only | Microbenchmarks |

---

## 9. Relationship to Dirty Page List Generational GC

### 9.1 Integration Points

1. **Write Barrier**: Combines dirty page list generational and incremental checks
   ```rust
   fn write_barrier(old, new) {
       dirty_page_barrier(new);   // From spec 007
       incremental_barrier(old);  // New for 0.8
   }
   ```

2. **Collection Scheduling**:
   - Minor GC: STW, uses dirty page list (spec 007)
   - Major GC: Incremental, uses write barrier + dirty page snapshots
   - Incremental slices are bounded by a global budget split per worker
   - All workers rendezvous at the slice barrier before yielding to mutators

3. **Phase Coordination**:
   - Cannot run minor GC during incremental major mark
   - Must complete or abort incremental mark before minor GC
   - Exceeding snapshot or budget thresholds triggers STW completion

### 9.2 Configuration

```rust
pub struct GcConfig {
    // From spec 007
    pub generational: bool,
    pub dirty_page_list: bool,
    
    // New for 0.8
    pub incremental_marking: bool,
    pub increment_size: usize,  // Objects per increment
    pub max_dirty_pages: usize,  // Snapshot size before fallback
    pub remembered_buffer_len: usize, // Per-thread write barrier buffer
    pub track_dirty_youngest_gen: bool, // Skip rescan when only old refs remain
}
```

---

## 10. Comparison with Original 0.8 Plan

| Aspect | Original 0.8 Plan | Revised 0.8.3 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Mutex-protected page lists | Dirty page list snapshots |
| **Write Barrier** | Mutex contention | Fast path + remembered buffer (Chez pattern) |
| **State Management** | Global state machine | Dirty page snapshots |
| **Slice Coordination** | Not specified | Per-worker budgets + slice barrier |
| **Dirty Hygiene** | Rescan on every pass | Track youngest generation, clear on clean |
| **Complexity** | High (Mutex, snapshots, double-checks) | Moderate (reuses existing infra) |
| **Thread Safety** | Mutex + double-check patterns | Mutex + double-check patterns |
| **Performance** | Write barrier regression | Targeted O(dirty_pages) scans |
| **Risk Level** | High | Medium (reuses proven pattern) |

---

## 11. References

- **ChezScheme GC**: Reference for incremental marking implementation
- **V8 GC**: Inspiration for idle-time incremental marking
- **Go GC**: Reference for concurrent marking with write barriers
- **Dirty Page List GC (spec 007)**: Prerequisite for this plan
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"

---

## 12. Open Questions

1. **Budget Size**: What increment_size provides best pause/throughput tradeoff?
2. **Remembered Buffer Size**: What default size balances overhead and contention?
3. **Dirty Gen Tracking**: Should youngest-gen tracking be per-page or per-snapshot?
4. **Fallback Thresholds**: What max_dirty_pages and budget timeout are acceptable?
5. **Idle-Time Collection**: Should we mark during idle time (like V8)?
6. **Parallel Incremental**: How to parallelize marking without losing increments?
7. **Feature Flag**: Should incremental be default-on or opt-in initially?

---

*Document generated: 2026-02-03*  
*Based on: Dybvig Review and ChezScheme Reference*  
*Prerequisite: Dirty Page List GC (spec 007) must be complete and stable*

**Version**: 0.8.3  
**Date**: 2026-02-03  
**Status**: Draft - Revised with ChezScheme-informed optimizations  
**Dependency**: Requires Generational GC 0.7.x Dirty Page List to be complete  
**Goal**: Reduce major GC pause times through incremental marking integrated with dirty page list tracking

---

## Changelog

### Version 0.8.3 (Revised)

**Key Changes from 0.8.2:**

1. **Parallel Slice Coordination**: Adds explicit per-worker budgets and slice barriers
2. **Write Barrier Fast Path**: Adds remembered-buffer and minimal checks on hot path
3. **Dirty Page Hygiene**: Tracks youngest-generation references to reduce rescan
4. **Snapshot Completion Criteria**: Completion requires worklist empty + dirty snapshot drained
5. **Fallback Thresholds**: Adds clear overflow/timeout fallback to STW completion

### Version 0.8.2 (Revised)

**Key Changes from 0.8.1:**

1. **Dirty Page List Integration**: Updated to work with mutex-protected dirty page lists from spec 007
2. **Snapshot Scanning**: Incremental marking scans dirty page snapshots rather than card tables
3. **Write Barrier Alignment**: Uses existing dirty page list barrier to record old-generation mutations
4. **ChezScheme Alignment**: Keeps the Chez Scheme dirty list + snapshot pattern
5. **Pragmatic Concurrency**: Prioritizes correctness over lock-free buffers in this phase
6. **Compatibility**: Matches current `rudo-gc` implementation of dirty pages in `LocalHeap`

---

## 1. Executive Summary

**Goal**: Implement incremental marking to reduce major collection pause times by splitting the mark phase into smaller, cooperative increments that interleave with mutator execution.

**Target Improvement**: 50-80% reduction in major GC pause times for large heaps by avoiding long stop-the-world marking phases.

**Key Challenge**: Maintaining correctness when objects are modified during the incremental mark phase (requires snapshot-at-the-beginning or incremental update algorithms).

**Timeline**: 3 weeks (after dirty page list generational GC is complete and stable)

---

## 2. Background: Why Incremental Marking?

### 2.1 Current Major Collection Flow

```
Major GC (Current):
1. Stop-the-world (STW)
2. Mark all reachable objects (O(live_objects))
3. Sweep dead objects
4. Resume mutators

Problem: Step 2 can take 100ms+ for large heaps (1GB+)
```

### 2.2 Incremental Marking Flow

```
Major GC (Incremental):
1. STW: Snapshot roots (short)
2. Resume mutators + Mark incrementally (cooperative)
3. STW: Finalize marking (short)
4. Sweep dead objects

Benefit: Mutator runs during most of marking, shorter pauses
```

### 2.3 The Mutator Problem

During incremental marking, mutators can:
- **Create new objects** (white - unmarked)
- **Modify references** (hide objects from marker)
- **Delete references** (expose objects that were hidden)

**Example of Lost Object**:
```
Before mark: A > B > C (C is live)
During mark: Mutator changes A to point to C, drops B (B becomes garbage)
Marker visits A, marks C, never sees B
After mark: B is incorrectly collected (BUG!)
```

**Solution**: Write barrier must record mutations during incremental mark.

---

## 3. Architecture Design

### 3.1 Snapshot-At-The-Beginning (SATB) with Dirty Page List Integration

We use a hybrid SATB + insertion-barrier approach integrated with our dirty page list
generational GC:

1. **Snapshot Phase**: Record all root references at mark start
2. **Incremental Mark**: Process worklist in bounded slices using dirty page snapshots
3. **Write Barrier**: Record overwritten references and immediately mark new values
4. **Slice Coordination**: Split a global budget into per-worker budgets with a slice barrier
5. **Completion Rule**: Marking completes only when worklist is empty and dirty snapshots drain
6. **Fallback**: If snapshots exceed thresholds or slice drift occurs, finish with STW

During MARKING, new allocations are treated as black (mark bit set on allocate) to avoid
missing newly created objects that never enter the worklist.

### 3.2 System Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                    Incremental Marking State Machine          │
├─────────────────────────────────────────────────────────────────────┤
│                                                               │
│  [IDLE] ─────────────────────────────────────────────────────────────────────┐
│                                │                             │
│                                ▼                             │
│                         (STW: short)                         │
│                         Capture roots                        │
│                         Clear marks                          │
│                         Set INCREMENTAL_MARK                 │
│                                │                             │
│                                ▼                             │
│                         [MARKING] ─────────────────────────────────────────────────────────────────────┐
│                         │                        │          │
│                         │ Mark chunks            │          │
│                         │ Check dirty pages      │          │
│                         │                        │          │
│                         ▼                        │          │
│                    Mark complete?                │          │
│                         │                        │          │
│                    Yes ├── No ─────────────────────────────────────────────────────────────────────┐
│                     │                                       │
│                     ▼                                       │
│               (STW: short)                                  │
│               Process final dirty pages                     │
│               Verify marking complete                       │
│                     │                                       │
│                     ▼                                       │
│               [SWEEPING]                                    │
│                     │                                       │
│                     ▼                                       │
│               [IDLE]                                        │
│                                                               │
└──────────────────────────────────────────────────────────────────────┘
```

### 3.3 Data Structures

#### 3.3.1 Incremental State with Dirty Page List Integration

**New File**: `src/gc/incremental.rs`

```rust
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::collections::VecDeque;

/// Global incremental marking state integrated with dirty page list tracking
pub struct IncrementalMarkState {
    /// Current phase of incremental marking
    phase: AtomicUsize,  // 0=IDLE, 1=SNAPSHOT, 2=MARKING, 3=FINAL_MARK, 4=SWEEPING
    
    /// Work queue for objects to mark (lock-free)
    worklist: crossbeam::queue::SegQueue<NonNull<GcBox<()>>>,
    
    /// Dirty page snapshot for incremental marking
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    /// Number of objects marked this increment
    marked_this_increment: AtomicUsize,
    
    /// Target: mark this many objects per increment
    increment_size: usize,
    
    /// Dirty page list (shared with generational GC)
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
}

/// Record of a reference write during incremental marking
/// (Retained for SATB if we need to track overwritten values)
pub struct DirtyRecord {
    /// Old value that was overwritten (needs to be marked)
    old_value: *const GcBox<()>,
}

/// Global singleton
static INCREMENTAL_STATE: OnceLock<IncrementalMarkState> = OnceLock::new();

/// GC phase constants
const PHASE_IDLE: usize = 0;
const PHASE_SNAPSHOT: usize = 1;
const PHASE_MARKING: usize = 2;
const PHASE_FINAL_MARK: usize = 3;
const PHASE_SWEEPING: usize = 4;
```

#### 3.3.2 GC Request Integration

**File**: `src/gc/gc.rs`

Add incremental flag to GC request:

```rust
pub struct GcRequest {
    pub collection_type: CollectionType,
    pub priority: GcPriority,
    pub incremental: bool,  // NEW: Request incremental collection
}

pub enum CollectionType {
    Minor,
    Major,
    IncrementalMajor,  // NEW: Incremental major collection
}
```

#### 3.3.3 Thread-Local Mark Tracking

**File**: `src/heap.rs`

```rust
pub struct ThreadControlBlock {
    // ... existing fields ...
    
    /// Local work queue for incremental marking
    /// Reduces contention on global worklist
    local_mark_queue: Vec<NonNull<GcBox<()>>>,
    
    /// Number of objects this thread has marked this increment
    marked_count: usize,

    /// Per-thread remembered buffer for write barrier batching
    remembered_buffer: Vec<NonNull<PageHeader>>,
}
```

### 3.4 Write Barrier for Incremental Marking with Dirty Page List Integration

#### 3.4.1 Enhanced Write Barrier

**File**: `src/cell.rs` and `src/gc/gc.rs`

The write barrier must:
1. Apply dirty page list generational barrier (old→young tracking)
2. Apply incremental barrier (record overwritten references)
3. Keep a fast path when incremental marking is off or the write is old→old
4. Optionally batch writes in a small remembered buffer to reduce lock contention

```rust
/// Combined write barrier for dirty page list generational + incremental GC
fn write_barrier<T: Trace>(&self, old_value: Option<&T>, new_value: &T) {
    // Fast path: if no incremental marking and not old->young, return early.
    if !is_incremental_marking_active()
        && !(self.is_old_generation() && new_value.is_young_generation())
    {
        return;
    }

    // 1. Dirty page list generational barrier (from spec 007)
    if self.is_old_generation() && new_value.is_young_generation() {
        // Optionally record into a small per-thread remembered buffer,
        // flushing to the global dirty list on overflow.
        add_to_dirty_pages(self.page());
    }

    // 2. Incremental barrier (new for 0.8)
    if is_incremental_marking_active() {
        if let Some(old) = old_value {
            // Record the OLD value that was overwritten.
            record_dirty_reference(old);
        }

        // Also mark the NEW value immediately (Dijkstra-style).
        mark_if_unmarked(new_value);
    }
}
```

#### 3.4.2 Dirty Page Snapshot Processing

**File**: `src/gc/incremental.rs`

```rust
impl IncrementalMarkState {
    /// Called during each marking increment
    pub fn process_dirty_pages(&mut self) {
        // Snapshot dirty pages for lock-free scanning
        let mut dirty_pages = self.dirty_pages.lock();
        self.dirty_pages_snapshot.clear();
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        drop(dirty_pages);
    }
    
    /// Record a dirty page (called from write barrier)
    pub fn record_dirty_page(&self, page: NonNull<PageHeader>) {
        let mut dirty_pages = self.dirty_pages.lock();
        dirty_pages.push(page);
    }
}
```

Implementation notes:
1. Track a per-page or per-snapshot "youngest generation referenced" to avoid rescanning pages
   that only contain old→old references.
2. If the snapshot grows beyond `max_dirty_pages`, fall back to STW completion to preserve
   correctness and avoid unbounded incremental overhead.
3. When a page is scanned and found clean (no young refs), clear its dirty flag immediately.

### 3.5 Incremental Mark Loop with Dirty Page List Integration

#### 3.5.1 Cooperative Yield Points

**File**: `src/gc/gc.rs`

```rust
/// Mark a chunk of objects, then yield.
/// Uses a global budget that is split into per-worker budgets to bound slice time.
pub fn mark_increment(heap: &LocalHeap, global_budget: usize) -> MarkStatus {
    let state = incremental_state();
    let local_budget = split_budget_per_worker(global_budget);
    let mut marked = 0;

    // 1. Process worklist up to budget
    while marked < local_budget {
        if let Some(ptr) = state.worklist.pop() {
            unsafe {
                let gc_box = ptr.as_ptr();
                if !(*gc_box).is_marked() {
                    (*gc_box).set_mark();
                    ((*gc_box).trace_fn)(ptr.as_ptr().cast(), &mut IncrementalVisitor);
                }
            }
            marked += 1;
        } else {
            // Worklist empty - scan dirty pages snapshot
            state.process_dirty_pages();
            if state.dirty_pages_snapshot.is_empty() {
                break;
            }
            for page in state.dirty_pages_snapshot.drain(..) {
                scan_dirty_page(page, &state.worklist);
            }
        }
    }

    // 2. Slice barrier: all workers rendezvous here to enforce slice boundary.
    // Work-stealing is allowed only within the local_budget window to avoid slice drift.
    slice_barrier_wait();

    if is_marking_complete() {
        MarkStatus::Complete
    } else {
        MarkStatus::Yield
    }
}

/// Check if marking is complete.
/// Completion requires both the global worklist and dirty snapshots to be empty.
pub fn is_marking_complete() -> bool {
    let state = incremental_state();
    state.worklist.is_empty() && state.dirty_pages_snapshot.is_empty()
}
```

#### 3.5.2 Mutator Yield Integration

**File**: `src/lib.rs`

Provide a cooperative yield function for long-running computations:

```rust
impl Gc {
    /// Yield to GC during long computations
    /// Call this periodically in loops that allocate heavily
    pub fn yield_now() {
        if is_incremental_marking_active() {
            // Allow GC to run an increment
            incremental_mark_step();
        }
    }
}
```

---

## 4. Implementation Phases

### Phase 1: State Management and Infrastructure (Week 1)

**Tasks**:
1. Create `src/gc/incremental.rs` with state management
2. Add phase constants and atomic state transitions
3. Implement worklist with crossbeam queue
4. Add dirty page snapshot buffer
5. Create `is_incremental_marking_active()` helper
6. Unit tests for state machine

**Deliverables**:
- `src/gc/incremental.rs` - Core incremental state
- `tests/incremental_state.rs` - State machine tests

### Phase 2: Write Barrier Integration (Week 1-2)

**Tasks**:
1. Enhance write barrier to support incremental marking with dirty page list integration
2. Implement dirty page snapshot recording
3. Add write barrier to GcCell, Gc<T>, and derived Trace impls
4. Add a small per-thread remembered buffer to batch dirty page updates
5. Tests for write barrier correctness

**Deliverables**:
- Modified `src/cell.rs` - Incremental write barrier
- Modified `src/gc/gc.rs` - Barrier integration
- Modified `rudo-gc-derive/` - Trace macro updates
- `tests/incremental_write_barrier.rs` - Barrier tests

### Phase 3: Mark Loop and Snapshots (Week 2)

**Tasks**:
1. Implement snapshot-at-beginning root capture
2. Create incremental mark loop with global + per-worker budgets
3. Integrate with existing parallel marking infrastructure (slice barrier)
4. Add dirty page snapshot processing
5. Mark completion detection

**Deliverables**:
- Modified `src/gc/marker.rs` - Incremental worker integration
- Modified `src/gc/gc.rs` - Incremental collection entry points
- `tests/incremental_marking.rs` - Mark loop tests

### Phase 4: Final Mark and Integration (Week 2-3)

**Tasks**:
1. Implement final mark phase (STW to complete marking)
2. Integration with dirty page list generational GC (spec 007)
3. Gc::yield_now() for cooperative scheduling
4. Configuration options (increment_size, enable/disable)
5. Full integration tests

**Deliverables**:
- Modified `src/lib.rs` - Public API and yield
- Modified `src/gc/gc.rs` - Integration
- `tests/incremental_integration.rs` - Full workflow tests
- `tests/incremental_generational.rs` - Combined GC tests

### Phase 5: Testing and Optimization (Week 3)

**Tasks**:
1. Run full test suite
2. Create pause time benchmarks
3. Profile and optimize hot paths
4. Stress tests for concurrent scenarios
5. Documentation

**Deliverables**:
- `benchmarks/incremental_pause.rs` - Pause benchmarks
- Performance comparison report
- Updated documentation

---

## 5. File Changes Summary

### 5.1 New Files (4)

| File | Purpose |
|------|---------|
| `src/gc/incremental.rs` | Core incremental marking state |
| `tests/incremental_state.rs` | State machine tests |
| `tests/incremental_write_barrier.rs` | Barrier tests |
| `tests/incremental_integration.rs` | Full workflow tests |

### 5.2 Modified Files (7)

| File | Changes |
|------|---------|
| `src/gc/gc.rs` | Incremental collection, write barrier integration |
| `src/cell.rs` | Enhanced write barrier |
| `src/heap.rs` | Thread-local mark queues |
| `src/gc/marker.rs` | Incremental worker support |
| `src/gc/worklist.rs` | Work-stealing for incremental |
| `src/lib.rs` | Gc::yield_now(), public API |
| `rudo-gc-derive/` | Trace macro updates |

---

## 6. Testing Strategy

### 6.1 Correctness Tests

```rust
// Example: Test that mutator changes don't lose objects
#[test]
fn test_incremental_no_lost_objects() {
    loom::model(|| {
        // Start incremental marking
        start_incremental_mark();
        
        let obj_a = Gc::new(Data { value: 1 });
        let obj_b = Gc::new(Data { value: 2 });
        let obj_c = Gc::new(Data { value: 3 });
        
        // Mutator modifies references during marking
        let cell = GcCell::new(obj_b);
        *cell.borrow_mut() = obj_c;  // Write barrier records this
        
        // Continue marking
        run_incremental_mark_to_completion();
        
        // All objects must survive
        assert!(!obj_a.is_dead());
        assert!(!obj_c.is_dead());  // obj_b can be collected
    });
}
```

### 6.2 Pause Time Benchmarks

```rust
// Example: Measure incremental vs STW pause times
fn bench_incremental_pause(c: &mut Criterion) {
    c.bench_function("stw_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_stw();
            start.elapsed()
        });
    });
    
    c.bench_function("incremental_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_incremental();
            // Measure longest single pause, not total time
            incremental_max_pause_time()
        });
    });
}
```

### 6.3 Stress Tests

- Concurrent allocation during incremental mark
- Heavy mutation rate during mark phase
- Large object graphs (deep trees, cycles)
- Multi-threaded mutators with shared data
- Parallel slice budget drift under work-stealing
- Remembered buffer overflow/flush ordering
- Dirty page youngest-generation tracking correctness

---

## 7. Risk Assessment

### 7.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Lost objects due to race | Medium | Critical | Extensive loom tests, SATB algorithm |
| Write barrier overhead | High | Medium | Profile hot path, optimize fast path |
| Memory bloat (dirty pages) | Medium | Medium | Bounded buffer, overflow to STW |
| Complexity in GC state | Medium | High | Clear state machine, thorough testing |
| Integration with generational | Medium | Medium | Test combined scenarios |
| Slice boundary drift (parallel marking) | Medium | High | Per-worker budgets + barrier sync |
| Remembered buffer overflow | Medium | Medium | Flush ordering tests, size defaults |
| Dirty youngest-gen tracking | Low | Medium | Verify rescan rules in loom tests |

### 7.2 Mitigation Strategies

1. **Formal Verification**: Use loom for all concurrent scenarios
2. **Bounded Buffers**: Dirty record buffer has max size; overflow triggers STW completion
3. **Gradual Rollout**: Feature flag for testing before default-on
4. **Fallback**: Can always fall back to STW marking if incremental fails

---

## 8. Success Criteria

### 8.1 Functional Requirements

- [ ] No lost objects during incremental marking (loom tests pass)
- [ ] No double-free or UAF (Miri clean)
- [ ] All existing tests pass (./test.sh)
- [ ] Write barrier correct under concurrent mutation
- [ ] Completion check requires worklist empty + dirty snapshot drained
- [ ] Slice budgets respected under parallel marking
- [ ] Graceful fallback to STW on overflow

### 8.2 Performance Requirements

| Metric | Target | Measurement |
|--------|--------|-------------|
| Max pause time (1GB heap) | < 10ms | Benchmarks |
| Total GC time | No more than 2x STW | Comparison benchmark |
| Mutator utilization | > 90% during mark | Instrumentation |
| Write barrier overhead | < 10% vs generational only | Microbenchmarks |

---

## 9. Relationship to Dirty Page List Generational GC

### 9.1 Integration Points

1. **Write Barrier**: Combines dirty page list generational and incremental checks
   ```rust
   fn write_barrier(old, new) {
       dirty_page_barrier(new);   // From spec 007
       incremental_barrier(old);  // New for 0.8
   }
   ```

2. **Collection Scheduling**:
   - Minor GC: STW, uses dirty page list (spec 007)
   - Major GC: Incremental, uses write barrier + dirty page snapshots
   - Incremental slices are bounded by a global budget split per worker
   - All workers rendezvous at the slice barrier before yielding to mutators

3. **Phase Coordination**:
   - Cannot run minor GC during incremental major mark
   - Must complete or abort incremental mark before minor GC
   - Exceeding snapshot or budget thresholds triggers STW completion

### 9.2 Configuration

```rust
pub struct GcConfig {
    // From spec 007
    pub generational: bool,
    pub dirty_page_list: bool,
    
    // New for 0.8
    pub incremental_marking: bool,
    pub increment_size: usize,  // Objects per increment
    pub max_dirty_pages: usize,  // Snapshot size before fallback
    pub remembered_buffer_len: usize, // Per-thread write barrier buffer
    pub track_dirty_youngest_gen: bool, // Skip rescan when only old refs remain
}
```

---

## 10. Comparison with Original 0.8 Plan

| Aspect | Original 0.8 Plan | Revised 0.8.3 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Mutex-protected page lists | Dirty page list snapshots |
| **Write Barrier** | Mutex contention | Fast path + remembered buffer (Chez pattern) |
| **State Management** | Global state machine | Dirty page snapshots |
| **Slice Coordination** | Not specified | Per-worker budgets + slice barrier |
| **Dirty Hygiene** | Rescan on every pass | Track youngest generation, clear on clean |
| **Complexity** | High (Mutex, snapshots, double-checks) | Moderate (reuses existing infra) |
| **Thread Safety** | Mutex + double-check patterns | Mutex + double-check patterns |
| **Performance** | Write barrier regression | Targeted O(dirty_pages) scans |
| **Risk Level** | High | Medium (reuses proven pattern) |

---

## 11. References

- **ChezScheme GC**: Reference for incremental marking implementation
- **V8 GC**: Inspiration for idle-time incremental marking
- **Go GC**: Reference for concurrent marking with write barriers
- **Dirty Page List GC (spec 007)**: Prerequisite for this plan
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"

---

## 12. Open Questions

1. **Budget Size**: What increment_size provides best pause/throughput tradeoff?
2. **Remembered Buffer Size**: What default size balances overhead and contention?
3. **Dirty Gen Tracking**: Should youngest-gen tracking be per-page or per-snapshot?
4. **Fallback Thresholds**: What max_dirty_pages and budget timeout are acceptable?
5. **Idle-Time Collection**: Should we mark during idle time (like V8)?
6. **Parallel Incremental**: How to parallelize marking without losing increments?
7. **Feature Flag**: Should incremental be default-on or opt-in initially?

---

*Document generated: 2026-02-03*  
*Based on: Dybvig Review and ChezScheme Reference*  
*Prerequisite: Dirty Page List GC (spec 007) must be complete and stable*

**Version**: 0.8.3  
**Date**: 2026-02-03  
**Status**: Draft - Revised with ChezScheme-informed optimizations  
**Dependency**: Requires Generational GC 0.7.x Dirty Page List to be complete  
**Goal**: Reduce major GC pause times through incremental marking integrated with dirty page list tracking

---

## Changelog

### Version 0.8.3 (Revised)

**Key Changes from 0.8.2:**

1. **Parallel Slice Coordination**: Adds explicit per-worker budgets and slice barriers
2. **Write Barrier Fast Path**: Adds remembered-buffer and minimal checks on hot path
3. **Dirty Page Hygiene**: Tracks youngest-generation references to reduce rescan
4. **Snapshot Completion Criteria**: Completion requires worklist empty + dirty snapshot drained
5. **Fallback Thresholds**: Adds clear overflow/timeout fallback to STW completion

### Version 0.8.2 (Revised)

**Key Changes from 0.8.1:**

1. **Dirty Page List Integration**: Updated to work with mutex-protected dirty page lists from spec 007
2. **Snapshot Scanning**: Incremental marking scans dirty page snapshots rather than card tables
3. **Write Barrier Alignment**: Uses existing dirty page list barrier to record old-generation mutations
4. **ChezScheme Alignment**: Keeps the Chez Scheme dirty list + snapshot pattern
5. **Pragmatic Concurrency**: Prioritizes correctness over lock-free buffers in this phase
6. **Compatibility**: Matches current `rudo-gc` implementation of dirty pages in `LocalHeap`

---

## 1. Executive Summary

**Goal**: Implement incremental marking to reduce major collection pause times by splitting the mark phase into smaller, cooperative increments that interleave with mutator execution.

**Target Improvement**: 50-80% reduction in major GC pause times for large heaps by avoiding long stop-the-world marking phases.

**Key Challenge**: Maintaining correctness when objects are modified during the incremental mark phase (requires snapshot-at-the-beginning or incremental update algorithms).

**Timeline**: 3 weeks (after dirty page list generational GC is complete and stable)

---

## 2. Background: Why Incremental Marking?

### 2.1 Current Major Collection Flow

```
Major GC (Current):
1. Stop-the-world (STW)
2. Mark all reachable objects (O(live_objects))
3. Sweep dead objects
4. Resume mutators

Problem: Step 2 can take 100ms+ for large heaps (1GB+)
```

### 2.2 Incremental Marking Flow

```
Major GC (Incremental):
1. STW: Snapshot roots (short)
2. Resume mutators + Mark incrementally (cooperative)
3. STW: Finalize marking (short)
4. Sweep dead objects

Benefit: Mutator runs during most of marking, shorter pauses
```

### 2.3 The Mutator Problem

During incremental marking, mutators can:
- **Create new objects** (white - unmarked)
- **Modify references** (hide objects from marker)
- **Delete references** (expose objects that were hidden)

**Example of Lost Object**:
```
Before mark: A > B > C (C is live)
During mark: Mutator changes A to point to C, drops B (B becomes garbage)
Marker visits A, marks C, never sees B
After mark: B is incorrectly collected (BUG!)
```

**Solution**: Write barrier must record mutations during incremental mark.

---

## 3. Architecture Design

### 3.1 Snapshot-At-The-Beginning (SATB) with Dirty Page List Integration

We use a hybrid SATB + insertion-barrier approach integrated with our dirty page list
generational GC:

1. **Snapshot Phase**: Record all root references at mark start
2. **Incremental Mark**: Process worklist in bounded slices using dirty page snapshots
3. **Write Barrier**: Record overwritten references and immediately mark new values
4. **Slice Coordination**: Split a global budget into per-worker budgets with a slice barrier
5. **Completion Rule**: Marking completes only when worklist is empty and dirty snapshots drain
6. **Fallback**: If snapshots exceed thresholds or slice drift occurs, finish with STW

During MARKING, new allocations are treated as black (mark bit set on allocate) to avoid
missing newly created objects that never enter the worklist.

### 3.2 System Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                    Incremental Marking State Machine          │
├─────────────────────────────────────────────────────────────────────┤
│                                                               │
│  [IDLE] ─────────────────────────────────────────────────────────────────────┐
│                                │                             │
│                                ▼                             │
│                         (STW: short)                         │
│                         Capture roots                        │
│                         Clear marks                          │
│                         Set INCREMENTAL_MARK                 │
│                                │                             │
│                                ▼                             │
│                         [MARKING] ─────────────────────────────────────────────────────────────────────┐
│                         │                        │          │
│                         │ Mark chunks            │          │
│                         │ Check dirty pages      │          │
│                         │                        │          │
│                         ▼                        │          │
│                    Mark complete?                │          │
│                         │                        │          │
│                    Yes ├── No ─────────────────────────────────────────────────────────────────────┐
│                     │                                       │
│                     ▼                                       │
│               (STW: short)                                  │
│               Process final dirty pages                     │
│               Verify marking complete                       │
│                     │                                       │
│                     ▼                                       │
│               [SWEEPING]                                    │
│                     │                                       │
│                     ▼                                       │
│               [IDLE]                                        │
│                                                               │
└──────────────────────────────────────────────────────────────────────┘
```

### 3.3 Data Structures

#### 3.3.1 Incremental State with Dirty Page List Integration

**New File**: `src/gc/incremental.rs`

```rust
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::collections::VecDeque;

/// Global incremental marking state integrated with dirty page list tracking
pub struct IncrementalMarkState {
    /// Current phase of incremental marking
    phase: AtomicUsize,  // 0=IDLE, 1=SNAPSHOT, 2=MARKING, 3=FINAL_MARK, 4=SWEEPING
    
    /// Work queue for objects to mark (lock-free)
    worklist: crossbeam::queue::SegQueue<NonNull<GcBox<()>>>,
    
    /// Dirty page snapshot for incremental marking
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    /// Number of objects marked this increment
    marked_this_increment: AtomicUsize,
    
    /// Target: mark this many objects per increment
    increment_size: usize,
    
    /// Dirty page list (shared with generational GC)
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
}

/// Record of a reference write during incremental marking
/// (Retained for SATB if we need to track overwritten values)
pub struct DirtyRecord {
    /// Old value that was overwritten (needs to be marked)
    old_value: *const GcBox<()>,
}

/// Global singleton
static INCREMENTAL_STATE: OnceLock<IncrementalMarkState> = OnceLock::new();

/// GC phase constants
const PHASE_IDLE: usize = 0;
const PHASE_SNAPSHOT: usize = 1;
const PHASE_MARKING: usize = 2;
const PHASE_FINAL_MARK: usize = 3;
const PHASE_SWEEPING: usize = 4;
```

#### 3.3.2 GC Request Integration

**File**: `src/gc/gc.rs`

Add incremental flag to GC request:

```rust
pub struct GcRequest {
    pub collection_type: CollectionType,
    pub priority: GcPriority,
    pub incremental: bool,  // NEW: Request incremental collection
}

pub enum CollectionType {
    Minor,
    Major,
    IncrementalMajor,  // NEW: Incremental major collection
}
```

#### 3.3.3 Thread-Local Mark Tracking

**File**: `src/heap.rs`

```rust
pub struct ThreadControlBlock {
    // ... existing fields ...
    
    /// Local work queue for incremental marking
    /// Reduces contention on global worklist
    local_mark_queue: Vec<NonNull<GcBox<()>>,
    
    /// Number of objects this thread has marked this increment
    marked_count: usize,
}
```

### 3.4 Write Barrier for Incremental Marking with Dirty Page List Integration

#### 3.4.1 Enhanced Write Barrier

**File**: `src/cell.rs` and `src/gc/gc.rs`

The write barrier must:
1. Apply dirty page list generational barrier (old→young tracking)
2. Apply incremental barrier (record overwritten references)
3. Keep a fast path when incremental marking is off or the write is old→old
4. Optionally batch writes in a small remembered buffer to reduce lock contention

```rust
/// Combined write barrier for dirty page list generational + incremental GC
fn write_barrier<T: Trace>(&self, old_value: Option<&T>, new_value: &T) {
    // Fast path: if no incremental marking and not old->young, return early.
    if !is_incremental_marking_active()
        && !(self.is_old_generation() && new_value.is_young_generation())
    {
        return;
    }

    // 1. Dirty page list generational barrier (from spec 007)
    if self.is_old_generation() && new_value.is_young_generation() {
        // Optionally record into a small per-thread remembered buffer,
        // flushing to the global dirty list on overflow.
        add_to_dirty_pages(self.page());
    }

    // 2. Incremental barrier (new for 0.8)
    if is_incremental_marking_active() {
        if let Some(old) = old_value {
            // Record the OLD value that was overwritten.
            record_dirty_reference(old);
        }

        // Also mark the NEW value immediately (Dijkstra-style).
        mark_if_unmarked(new_value);
    }
}
```

#### 3.4.2 Dirty Page Snapshot Processing

**File**: `src/gc/incremental.rs`

```rust
impl IncrementalMarkState {
    /// Called during each marking increment
    pub fn process_dirty_pages(&mut self) {
        // Snapshot dirty pages for lock-free scanning
        let mut dirty_pages = self.dirty_pages.lock();
        self.dirty_pages_snapshot.clear();
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        drop(dirty_pages);
    }
    
    /// Record a dirty page (called from write barrier)
    pub fn record_dirty_page(&self, page: NonNull<PageHeader>) {
        let mut dirty_pages = self.dirty_pages.lock();
        dirty_pages.push(page);
    }
}
```

Implementation notes:
1. Track a per-page or per-snapshot "youngest generation referenced" to avoid rescanning pages
   that only contain old→old references.
2. If the snapshot grows beyond `max_dirty_pages`, fall back to STW completion to preserve
   correctness and avoid unbounded incremental overhead.
3. When a page is scanned and found clean (no young refs), clear its dirty flag immediately.

### 3.5 Incremental Mark Loop with Dirty Page List Integration

#### 3.5.1 Cooperative Yield Points

**File**: `src/gc/gc.rs`

```rust
/// Mark a chunk of objects, then yield.
/// Uses a global budget that is split into per-worker budgets to bound slice time.
pub fn mark_increment(heap: &LocalHeap, global_budget: usize) -> MarkStatus {
    let state = incremental_state();
    let local_budget = split_budget_per_worker(global_budget);
    let mut marked = 0;

    // 1. Process worklist up to budget
    while marked < local_budget {
        if let Some(ptr) = state.worklist.pop() {
            unsafe {
                let gc_box = ptr.as_ptr();
                if !(*gc_box).is_marked() {
                    (*gc_box).set_mark();
                    ((*gc_box).trace_fn)(ptr.as_ptr().cast(), &mut IncrementalVisitor);
                }
            }
            marked += 1;
        } else {
            // Worklist empty - scan dirty pages snapshot
            state.process_dirty_pages();
            if state.dirty_pages_snapshot.is_empty() {
                break;
            }
            for page in state.dirty_pages_snapshot.drain(..) {
                scan_dirty_page(page, &state.worklist);
            }
        }
    }

    // 2. Slice barrier: all workers rendezvous here to enforce slice boundary.
    // Work-stealing is allowed only within the local_budget window to avoid slice drift.
    slice_barrier_wait();

    if is_marking_complete() {
        MarkStatus::Complete
    } else {
        MarkStatus::Yield
    }
}

/// Check if marking is complete.
/// Completion requires both the global worklist and dirty snapshots to be empty.
pub fn is_marking_complete() -> bool {
    let state = incremental_state();
    state.worklist.is_empty() && state.dirty_pages_snapshot.is_empty()
}
```

#### 3.5.2 Mutator Yield Integration

**File**: `src/lib.rs`

Provide a cooperative yield function for long-running computations:

```rust
impl Gc {
    /// Yield to GC during long computations
    /// Call this periodically in loops that allocate heavily
    pub fn yield_now() {
        if is_incremental_marking_active() {
            // Allow GC to run an increment
            incremental_mark_step();
        }
    }
}
```

---

## 4. Implementation Phases

### Phase 1: State Management and Infrastructure (Week 1)

**Tasks**:
1. Create `src/gc/incremental.rs` with state management
2. Add phase constants and atomic state transitions
3. Implement worklist with crossbeam queue
4. Add dirty page snapshot buffer
5. Create `is_incremental_marking_active()` helper
6. Unit tests for state machine

**Deliverables**:
- `src/gc/incremental.rs` - Core incremental state
- `tests/incremental_state.rs` - State machine tests

### Phase 2: Write Barrier Integration (Week 1-2)

**Tasks**:
1. Enhance write barrier to support incremental marking with dirty page list integration
2. Implement dirty page snapshot recording
3. Add write barrier to GcCell, Gc<T>, and derived Trace impls
4. Add a small per-thread remembered buffer to batch dirty page updates
5. Tests for write barrier correctness

**Deliverables**:
- Modified `src/cell.rs` - Incremental write barrier
- Modified `src/gc/gc.rs` - Barrier integration
- Modified `rudo-gc-derive/` - Trace macro updates
- `tests/incremental_write_barrier.rs` - Barrier tests

### Phase 3: Mark Loop and Snapshots (Week 2)

**Tasks**:
1. Implement snapshot-at-beginning root capture
2. Create incremental mark loop with global + per-worker budgets
3. Integrate with existing parallel marking infrastructure (slice barrier)
4. Add dirty page snapshot processing
5. Mark completion detection

**Deliverables**:
- Modified `src/gc/marker.rs` - Incremental worker integration
- Modified `src/gc/gc.rs` - Incremental collection entry points
- `tests/incremental_marking.rs` - Mark loop tests

### Phase 4: Final Mark and Integration (Week 2-3)

**Tasks**:
1. Implement final mark phase (STW to complete marking)
2. Integration with dirty page list generational GC (spec 007)
3. Gc::yield_now() for cooperative scheduling
4. Configuration options (increment_size, enable/disable)
5. Full integration tests

**Deliverables**:
- Modified `src/lib.rs` - Public API and yield
- Modified `src/gc/gc.rs` - Integration
- `tests/incremental_integration.rs` - Full workflow tests
- `tests/incremental_generational.rs` - Combined GC tests

### Phase 5: Testing and Optimization (Week 3)

**Tasks**:
1. Run full test suite
2. Create pause time benchmarks
3. Profile and optimize hot paths
4. Stress tests for concurrent scenarios
5. Documentation

**Deliverables**:
- `benchmarks/incremental_pause.rs` - Pause benchmarks
- Performance comparison report
- Updated documentation

---

## 5. File Changes Summary

### 5.1 New Files (4)

| File | Purpose |
|------|---------|
| `src/gc/incremental.rs` | Core incremental marking state |
| `tests/incremental_state.rs` | State machine tests |
| `tests/incremental_write_barrier.rs` | Barrier tests |
| `tests/incremental_integration.rs` | Full workflow tests |

### 5.2 Modified Files (7)

| File | Changes |
|------|---------|
| `src/gc/gc.rs` | Incremental collection, write barrier integration |
| `src/cell.rs` | Enhanced write barrier |
| `src/heap.rs` | Thread-local mark queues |
| `src/gc/marker.rs` | Incremental worker support |
| `src/gc/worklist.rs` | Work-stealing for incremental |
| `src/lib.rs` | Gc::yield_now(), public API |
| `rudo-gc-derive/` | Trace macro updates |

---

## 6. Testing Strategy

### 6.1 Correctness Tests

```rust
// Example: Test that mutator changes don't lose objects
#[test]
fn test_incremental_no_lost_objects() {
    loom::model(|| {
        // Start incremental marking
        start_incremental_mark();
        
        let obj_a = Gc::new(Data { value: 1 });
        let obj_b = Gc::new(Data { value: 2 });
        let obj_c = Gc::new(Data { value: 3 });
        
        // Mutator modifies references during marking
        let cell = GcCell::new(obj_b);
        *cell.borrow_mut() = obj_c;  // Write barrier records this
        
        // Continue marking
        run_incremental_mark_to_completion();
        
        // All objects must survive
        assert!(!obj_a.is_dead());
        assert!(!obj_c.is_dead());  // obj_b can be collected
    });
}
```

### 6.2 Pause Time Benchmarks

```rust
// Example: Measure incremental vs STW pause times
fn bench_incremental_pause(c: &mut Criterion) {
    c.bench_function("stw_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_stw();
            start.elapsed()
        });
    });
    
    c.bench_function("incremental_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_incremental();
            // Measure longest single pause, not total time
            incremental_max_pause_time()
        });
    });
}
```

### 6.3 Stress Tests

- Concurrent allocation during incremental mark
- Heavy mutation rate during mark phase
- Large object graphs (deep trees, cycles)
- Multi-threaded mutators with shared data
- Parallel slice budget drift under work-stealing
- Remembered buffer overflow/flush ordering
- Dirty page youngest-generation tracking correctness

---

## 7. Risk Assessment

### 7.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Lost objects due to race | Medium | Critical | Extensive loom tests, SATB algorithm |
| Write barrier overhead | High | Medium | Profile hot path, optimize fast path |
| Memory bloat (dirty pages) | Medium | Medium | Bounded buffer, overflow to STW |
| Complexity in GC state | Medium | High | Clear state machine, thorough testing |
| Integration with generational | Medium | Medium | Test combined scenarios |
| Slice boundary drift (parallel marking) | Medium | High | Per-worker budgets + barrier sync |
| Remembered buffer overflow | Medium | Medium | Flush ordering tests, size defaults |
| Dirty youngest-gen tracking | Low | Medium | Verify rescan rules in loom tests |

### 7.2 Mitigation Strategies

1. **Formal Verification**: Use loom for all concurrent scenarios
2. **Bounded Buffers**: Dirty record buffer has max size; overflow triggers STW completion
3. **Gradual Rollout**: Feature flag for testing before default-on
4. **Fallback**: Can always fall back to STW marking if incremental fails

---

## 8. Success Criteria

### 8.1 Functional Requirements

- [ ] No lost objects during incremental marking (loom tests pass)
- [ ] No double-free or UAF (Miri clean)
- [ ] All existing tests pass (./test.sh)
- [ ] Write barrier correct under concurrent mutation
- [ ] Completion check requires worklist empty + dirty snapshot drained
- [ ] Slice budgets respected under parallel marking
- [ ] Graceful fallback to STW on overflow

### 8.2 Performance Requirements

| Metric | Target | Measurement |
|--------|--------|-------------|
| Max pause time (1GB heap) | < 10ms | Benchmarks |
| Total GC time | No more than 2x STW | Comparison benchmark |
| Mutator utilization | > 90% during mark | Instrumentation |
| Write barrier overhead | < 10% vs generational only | Microbenchmarks |

---

## 9. Relationship to Dirty Page List Generational GC

### 9.1 Integration Points

1. **Write Barrier**: Combines dirty page list generational and incremental checks
   ```rust
   fn write_barrier(old, new) {
       dirty_page_barrier(new);   // From spec 007
       incremental_barrier(old);  // New for 0.8
   }
   ```

2. **Collection Scheduling**:
   - Minor GC: STW, uses dirty page list (spec 007)
   - Major GC: Incremental, uses write barrier + dirty page snapshots
   - Incremental slices are bounded by a global budget split per worker
   - All workers rendezvous at the slice barrier before yielding to mutators

3. **Phase Coordination**:
   - Cannot run minor GC during incremental major mark
   - Must complete or abort incremental mark before minor GC
   - Exceeding snapshot or budget thresholds triggers STW completion

### 9.2 Configuration

```rust
pub struct GcConfig {
    // From spec 007
    pub generational: bool,
    pub dirty_page_list: bool,
    
    // New for 0.8
    pub incremental_marking: bool,
    pub increment_size: usize,  // Objects per increment
    pub max_dirty_pages: usize,  // Snapshot size before fallback
    pub remembered_buffer_len: usize, // Per-thread write barrier buffer
    pub track_dirty_youngest_gen: bool, // Skip rescan when only old refs remain
}
```

---

## 10. Comparison with Original 0.8 Plan

| Aspect | Original 0.8 Plan | Revised 0.8.3 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Mutex-protected page lists | Dirty page list snapshots |
| **Write Barrier** | Mutex contention | Fast path + remembered buffer (Chez pattern) |
| **State Management** | Global state machine | Dirty page snapshots |
| **Slice Coordination** | Not specified | Per-worker budgets + slice barrier |
| **Dirty Hygiene** | Rescan on every pass | Track youngest generation, clear on clean |
| **Complexity** | High (Mutex, snapshots, double-checks) | Moderate (reuses existing infra) |
| **Thread Safety** | Mutex + double-check patterns | Mutex + double-check patterns |
| **Performance** | Write barrier regression | Targeted O(dirty_pages) scans |
| **Risk Level** | High | Medium (reuses proven pattern) |

---

## 11. References

- **ChezScheme GC**: Reference for incremental marking implementation
- **V8 GC**: Inspiration for idle-time incremental marking
- **Go GC**: Reference for concurrent marking with write barriers
- **Dirty Page List GC (spec 007)**: Prerequisite for this plan
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"

---

## 12. Open Questions

1. **Budget Size**: What increment_size provides best pause/throughput tradeoff?
2. **Dirty Record Buffer**: How large before overflow is acceptable?
3. **Idle-Time Collection**: Should we mark during idle time (like V8)?
4. **Parallel Incremental**: How to parallelize marking without losing increments?
5. **Feature Flag**: Should incremental be default-on or opt-in initially?

---

*Document generated: 2026-02-03*  
*Based on: Dybvig Review and ChezScheme Reference*  
*Prerequisite: Dirty Page List GC (spec 007) must be complete and stable*

1. **Snapshot Phase**: Record all root references at mark start
2. **Incremental Mark**: Process worklist in small chunks
3. **Write Barrier**: Record overwritten references (not new values)
4. **Final Mark**: Revisit recorded references to complete marking

### 3.2 System Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Incremental Marking State Machine          │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│  [IDLE] ──collect_major()──> [SNAPSHOT]                     │
│                                │                             │
│                                ▼                             │
│                         (STW: short)                         │
│                         Capture roots                        │
│                         Clear marks                          │
│                         Set INCREMENTAL_MARK                 │
│                                │                             │
│                                ▼                             │
│                         [MARKING] ──yield_now()──┐          │
│                         │                        │          │
│                         │ Mark chunks            │          │
│                         │ Check dirty pages      │          │
│                         │                        │          │
│                         ▼                        │          │
│                    Mark complete?                │          │
│                         │                        │          │
│                    Yes ─┴─ No ───────────────────┘          │
│                     │                                       │
│                     ▼                                       │
│               (STW: short)                                  │
│               Process final dirty pages                     │
│               Verify marking complete                       │
│                     │                                       │
│                     ▼                                       │
│               [SWEEPING]                                    │
│                     │                                       │
│                     ▼                                       │
│               [IDLE]                                        │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

### 3.3 Data Structures

#### 3.3.1 Incremental State

**New File**: `src/gc/incremental.rs`

```rust
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};

/// Global incremental marking state
pub struct IncrementalMarkState {
    /// Current phase of incremental marking
    phase: AtomicUsize,  // 0=IDLE, 1=SNAPSHOT, 2=MARKING, 3=FINAL_MARK, 4=SWEEPING
    
    /// Work queue for objects to mark
    worklist: crossbeam::queue::SegQueue<NonNull<GcBox<()>>>,
    
    /// Number of objects marked this increment
    marked_this_increment: AtomicUsize,
    
    /// Target: mark this many objects per increment
    increment_size: usize,
    
    /// Dirty page snapshot for incremental marking
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    /// Dirty page list (shared with generational GC)
    dirty_pages: Mutex<Vec<NonNull<PageHeader>>>,
}

/// Record of a reference write during incremental marking
pub struct DirtyRecord {
    /// Old value that was overwritten (needs to be marked)
    old_value: *const GcBox<()>,
}

/// Global singleton
static INCREMENTAL_STATE: OnceLock<IncrementalMarkState> = OnceLock::new();

/// GC phase constants
const PHASE_IDLE: usize = 0;
const PHASE_SNAPSHOT: usize = 1;
const PHASE_MARKING: usize = 2;
const PHASE_FINAL_MARK: usize = 3;
const PHASE_SWEEPING: usize = 4;
```

#### 3.3.2 GC Request Integration

**File**: `src/gc/gc.rs`

Add incremental flag to GC request:

```rust
pub struct GcRequest {
    pub collection_type: CollectionType,
    pub priority: GcPriority,
    pub incremental: bool,  // NEW: Request incremental collection
}

pub enum CollectionType {
    Minor,
    Major,
    IncrementalMajor,  // NEW: Incremental major collection
}
```

#### 3.3.3 Thread-Local Mark Tracking

**File**: `src/heap.rs`

```rust
pub struct ThreadControlBlock {
    // ... existing fields ...
    
    /// Local work queue for incremental marking
    /// Reduces contention on global worklist
    local_mark_queue: Vec<NonNull<GcBox<()>>>,
    
    /// Number of objects this thread has marked this increment
    marked_count: usize,
}
```

### 3.4 Write Barrier for Incremental Marking

#### 3.4.1 Enhanced Write Barrier

**File**: `src/cell.rs` and `src/gc/gc.rs`

The write barrier must:
1. Apply generational write barrier (old→young tracking)
2. Apply incremental write barrier (record overwritten references)

```rust
/// Combined write barrier for generational + incremental GC
fn write_barrier<T: Trace>(&self, old_value: Option<&T>, new_value: &T) {
    // 1. Generational barrier (from 0.7.x)
    // If old object writing young reference, mark page dirty
    if self.is_old_generation() && new_value.is_young_generation() {
        mark_page_dirty(self.page());
    }
    
    // 2. Incremental barrier (new for 0.8)
    // If incremental marking is active, record overwritten reference
    if is_incremental_marking_active() {
        if let Some(old) = old_value {
            // Record the OLD value that was overwritten
            // This ensures the old value gets marked even if no longer reachable
            record_dirty_reference(old);
        }
        
        // Also mark the NEW value immediately (Dijkstra-style)
        mark_if_unmarked(new_value);
    }
}
```

#### 3.4.2 Dirty Page Snapshot Processing

**File**: `src/gc/incremental.rs`

```rust
impl IncrementalMarkState {
    /// Called during each marking increment
    pub fn process_dirty_pages(&mut self) {
        // Snapshot dirty pages for lock-free scanning
        let mut dirty_pages = self.dirty_pages.lock().unwrap();
        self.dirty_pages_snapshot.clear();
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        drop(dirty_pages);
    }
    
    /// Record a dirty page (called from write barrier)
    pub fn record_dirty_page(&self, page: NonNull<PageHeader>) {
        self.dirty_pages.lock().unwrap().push(page);
    }
}
```

### 3.5 Incremental Mark Loop

#### 3.5.1 Cooperative Yield Points

**File**: `src/gc/gc.rs`

```rust
/// Mark a chunk of objects, then yield
pub fn mark_increment(heap: &LocalHeap, budget: usize) -> MarkStatus {
    let state = incremental_state();
    let mut marked = 0;
    
    // 1. Process worklist up to budget
    while marked < budget {
        if let Some(ptr) = state.worklist.pop() {
            unsafe {
                // Mark this object
                let gc_box = ptr.as_ptr();
                if !(*gc_box).is_marked() {
                    (*gc_box).set_mark();
                    
                    // Trace children and push to worklist
                    ((*gc_box).trace_fn)(ptr.as_ptr().cast(), &mut IncrementalVisitor);
                }
            }
            marked += 1;
        } else {
            // Worklist empty - check dirty pages
            state.process_dirty_pages();
            if state.dirty_pages_snapshot.is_empty() {
                // Nothing more to do
                return MarkStatus::Complete;
            }
            for page in state.dirty_pages_snapshot.drain(..) {
                scan_dirty_page(page, &state.worklist);
            }
        }
    }
    
    MarkStatus::Yield
}

/// Check if marking is complete
pub fn is_marking_complete() -> bool {
    let state = incremental_state();
    state.worklist.is_empty()
}
```

#### 3.5.2 Mutator Yield Integration

**File**: `src/lib.rs`

Provide a cooperative yield function for long-running computations:

```rust
impl Gc {
    /// Yield to GC during long computations
    /// Call this periodically in loops that allocate heavily
    pub fn yield_now() {
        if is_incremental_marking_active() {
            // Allow GC to run an increment
            incremental_mark_step();
        }
    }
}
```

---

## 4. Implementation Phases

### Phase 1: State Management and Infrastructure (Week 1)

**Tasks**:
1. Create `src/gc/incremental.rs` with state management
2. Add phase constants and atomic state transitions
3. Implement worklist with crossbeam queue
4. Add dirty page snapshot buffer
5. Create `is_incremental_marking_active()` helper
6. Unit tests for state machine

**Deliverables**:
- `src/gc/incremental.rs` - Core incremental state
- `tests/incremental_state.rs` - State machine tests

### Phase 2: Write Barrier Integration (Week 1-2)

**Tasks**:
1. Enhance write barrier to support incremental marking
2. Implement dirty page snapshot recording
3. Add write barrier to GcCell, Gc<T>, and derived Trace impls
4. Thread-local mark queue for reduced contention
5. Tests for write barrier correctness

**Deliverables**:
- Modified `src/cell.rs` - Incremental write barrier
- Modified `src/gc/gc.rs` - Barrier integration
- Modified `rudo-gc-derive/` - Trace macro updates
- `tests/incremental_write_barrier.rs` - Barrier tests

### Phase 3: Mark Loop and Snapshots (Week 2)

**Tasks**:
1. Implement snapshot-at-beginning root capture
2. Create incremental mark loop with budget
3. Integrate with existing parallel marking infrastructure
4. Add dirty page snapshot processing
5. Mark completion detection

**Deliverables**:
- Modified `src/gc/marker.rs` - Incremental worker integration
- Modified `src/gc/gc.rs` - Incremental collection entry points
- `tests/incremental_marking.rs` - Mark loop tests

### Phase 4: Final Mark and Integration (Week 2-3)

**Tasks**:
1. Implement final mark phase (STW to complete marking)
2. Integration with generational GC (0.7.x)
3. Gc::yield_now() for cooperative scheduling
4. Configuration options (increment_size, enable/disable)
5. Full integration tests

**Deliverables**:
- Modified `src/lib.rs` - Public API and yield
- Modified `src/gc/gc.rs` - Integration
- `tests/incremental_integration.rs` - Full workflow tests
- `tests/incremental_generational.rs` - Combined GC tests

### Phase 5: Testing and Optimization (Week 3)

**Tasks**:
1. Run full test suite
2. Create pause time benchmarks
3. Profile and optimize hot paths
4. Stress tests for concurrent scenarios
5. Documentation

**Deliverables**:
- `benchmarks/incremental_pause.rs` - Pause benchmarks
- Performance comparison report
- Updated documentation

---

## 5. File Changes Summary

### 5.1 New Files (4)

| File | Purpose |
|------|---------|
| `src/gc/incremental.rs` | Core incremental marking state |
| `tests/incremental_state.rs` | State machine tests |
| `tests/incremental_write_barrier.rs` | Barrier tests |
| `tests/incremental_integration.rs` | Full workflow tests |

### 5.2 Modified Files (7)

| File | Changes |
|------|---------|
| `src/gc/gc.rs` | Incremental collection, write barrier integration |
| `src/cell.rs` | Enhanced write barrier |
| `src/heap.rs` | Thread-local mark queues |
| `src/gc/marker.rs` | Incremental worker support |
| `src/gc/worklist.rs` | Work-stealing for incremental |
| `src/lib.rs` | Gc::yield_now(), public API |
| `rudo-gc-derive/` | Trace macro updates |

---

## 6. Testing Strategy

### 6.1 Correctness Tests

```rust
// Example: Test that mutator changes don't lose objects
#[test]
fn test_incremental_no_lost_objects() {
    loom::model(|| {
        // Start incremental marking
        start_incremental_mark();
        
        let obj_a = Gc::new(Data { value: 1 });
        let obj_b = Gc::new(Data { value: 2 });
        let obj_c = Gc::new(Data { value: 3 });
        
        // Mutator modifies references during marking
        let cell = GcCell::new(obj_b);
        *cell.borrow_mut() = obj_c;  // Write barrier records this
        
        // Continue marking
        run_incremental_mark_to_completion();
        
        // All objects must survive
        assert!(!obj_a.is_dead());
        assert!(!obj_c.is_dead());  // obj_b can be collected
    });
}
```

### 6.2 Pause Time Benchmarks

```rust
// Example: Measure incremental vs STW pause times
fn bench_incremental_pause(c: &mut Criterion) {
    c.bench_function("stw_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_stw();
            start.elapsed()
        });
    });
    
    c.bench_function("incremental_major_gc_1gb", |b| {
        b.iter(|| {
            allocate_1gb_heap();
            let start = Instant::now();
            collect_major_incremental();
            // Measure longest single pause, not total time
            incremental_max_pause_time()
        });
    });
}
```

### 6.3 Stress Tests

- Concurrent allocation during incremental mark
- Heavy mutation rate during mark phase
- Large object graphs (deep trees, cycles)
- Multi-threaded mutators with shared data

---

## 7. Risk Assessment

### 7.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Lost objects due to race | Medium | Critical | Extensive loom tests, SATB algorithm |
| Write barrier overhead | High | Medium | Profile hot path, optimize fast path |
| Memory bloat (dirty pages) | Medium | Medium | Bounded buffer, overflow to STW |
| Complexity in GC state | Medium | High | Clear state machine, thorough testing |
| Integration with generational | Medium | Medium | Test combined scenarios |

### 7.2 Mitigation Strategies

1. **Formal Verification**: Use loom for all concurrent scenarios
2. **Bounded Buffers**: Dirty record buffer has max size; overflow triggers STW completion
3. **Gradual Rollout**: Feature flag for testing before default-on
4. **Fallback**: Can always fall back to STW marking if incremental fails

---

## 8. Success Criteria

### 8.1 Functional Requirements

- [ ] No lost objects during incremental marking (loom tests pass)
- [ ] No double-free or UAF (Miri clean)
- [ ] All existing tests pass (./test.sh)
- [ ] Write barrier correct under concurrent mutation
- [ ] Graceful fallback to STW on overflow

### 8.2 Performance Requirements

| Metric | Target | Measurement |
|--------|--------|-------------|
| Max pause time (1GB heap) | < 10ms | Benchmarks |
| Total GC time | No more than 2x STW | Comparison benchmark |
| Mutator utilization | > 90% during mark | Instrumentation |
| Write barrier overhead | < 10% vs generational only | Microbenchmarks |

---

### 9.1 Integration Points

1. **Write Barrier**: Combines dirty page list generational and incremental checks
   ```rust
   fn write_barrier(old, new) {
       dirty_page_barrier(new);   // From spec 007
       incremental_barrier(old);  // New for 0.8
   }
   ```

2. **Collection Scheduling**:
   - Minor GC: STW, uses dirty page list (spec 007)
   - Major GC: Incremental, uses write barrier + dirty page snapshots

3. **Phase Coordination**:
   - Cannot run minor GC during incremental major mark
   - Must complete or abort incremental mark before minor GC

### 9.2 Configuration

```rust
pub struct GcConfig {
    // From spec 007
    pub generational: bool,
    pub dirty_page_list: bool,
    
    // New for 0.8
    pub incremental_marking: bool,
    pub increment_size: usize,  // Objects per increment
    pub max_dirty_pages: usize,  // Snapshot size before fallback
}
```

---

## 10. Comparison with Original 0.8 Plan

| Aspect | Original 0.8 Plan | Revised 0.8.2 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Mutex-protected page lists | Dirty page list snapshots |
| **Write Barrier** | Mutex contention | Mutex + double-check (Chez pattern) |
| **State Management** | Global state machine | Dirty page snapshots |
| **Complexity** | High (Mutex, snapshots, double-checks) | Moderate (reuses existing infra) |
| **Thread Safety** | Mutex + double-check patterns | Mutex + double-check patterns |
| **Performance** | Write barrier regression | Targeted O(dirty_pages) scans |
| **Risk Level** | High | Medium (reuses proven pattern) |

---

## 11. References

- **ChezScheme GC**: Reference for incremental marking implementation
- **V8 GC**: Inspiration for idle-time incremental marking
- **Go GC**: Reference for concurrent marking with write barriers
- **Dirty Page List GC (spec 007)**: Prerequisite for this plan
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"

---

## 12. Open Questions

1. **Budget Size**: What increment_size provides best pause/throughput tradeoff?
2. **Dirty Record Buffer**: How large before overflow is acceptable?
3. **Idle-Time Collection**: Should we mark during idle time (like V8)?
4. **Parallel Incremental**: How to parallelize marking without losing increments?
5. **Feature Flag**: Should incremental be default-on or opt-in initially?

---

*Document generated: 2026-02-03*  
*Based on: Dybvig Review and ChezScheme Reference*  
*Prerequisite: Dirty Page List GC (spec 007) must be complete and stable*

---

## Changelog

### Version 0.8.2 (Revised)

**Key Changes from 0.8.1:**

1. **Dirty Page List Integration**: Updated to work with mutex-protected dirty page lists from spec 007
2. **Snapshot Scanning**: Incremental marking scans dirty page snapshots rather than card tables
3. **Write Barrier Alignment**: Uses existing dirty page list barrier to record old-generation mutations
4. **ChezScheme Alignment**: Keeps the Chez Scheme dirty list + snapshot pattern
5. **Pragmatic Concurrency**: Prioritizes correctness over lock-free buffers in this phase
6. **Compatibility**: Matches current `rudo-gc` implementation of dirty pages in `LocalHeap`
