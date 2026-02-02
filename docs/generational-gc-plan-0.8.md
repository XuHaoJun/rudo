# rudo-gc Incremental Marking Implementation Plan

**Version**: 0.8.1  
**Date**: 2026-02-02  
**Status**: Draft - Revised for Card-Based Integration  
**Dependency**: Requires Generational GC 0.7.2 (Card-Based) to be complete  
**Goal**: Reduce major GC pause times through incremental marking integrated with card-based generational GC

---

## Changelog

### Version 0.8.1 (Revised)

**Key Changes from 0.8:**

1. **Card-Based Integration**: Updated to work with the card-based dirty tracking from 0.7.2 instead of mutex-protected page lists
2. **Simplified State Management**: Reduced complexity by leveraging per-segment dirty tracking instead of global state machines
3. **Optimized Write Barrier**: Combined generational and incremental checks without mutex contention
4. **ChezScheme Alignment**: Adopted proven patterns from ChezScheme's incremental marking implementation
5. **Lock-Free Design**: Replaced Mutex-based dirty record buffers with lock-free approaches
6. **Better Performance**: Eliminated write barrier contention while maintaining correctness guarantees

---

## 1. Executive Summary

**Goal**: Implement incremental marking to reduce major collection pause times by splitting the mark phase into smaller, cooperative increments that interleave with mutator execution.

**Target Improvement**: 50-80% reduction in major GC pause times for large heaps by avoiding long stop-the-world marking phases.

**Key Challenge**: Maintaining correctness when objects are modified during the incremental mark phase (requires snapshot-at-the-beginning or incremental update algorithms).

**Timeline**: 3 weeks (after 0.7.2 card-based generational GC is complete and stable)

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

### 3.1 Snapshot-At-The-Beginning (SATB) with Card-Based Integration

We use the Dijkstra-style incremental update approach integrated with our card-based generational GC:

1. **Snapshot Phase**: Record all root references at mark start
2. **Incremental Mark**: Process worklist in small chunks using card-based dirty tracking
3. **Write Barrier**: Record overwritten references (not new values) using lock-free approaches
4. **Final Mark**: Revisit recorded references to complete marking using card-based dirty segments

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
│                         │ Check dirty records    │          │
│                         │                        │          │
│                         ▼                        │          │
│                    Mark complete?                │          │
│                         │                        │          │
│                    Yes ├── No ─────────────────────────────────────────────────────────────────────┐
│                     │                                       │
│                     ▼                                       │
│               (STW: short)                                  │
│               Process final dirty records                   │
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

#### 3.3.1 Incremental State with Card-Based Integration

**New File**: `src/gc/incremental.rs`

```rust
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::collections::VecDeque;

/// Global incremental marking state integrated with card-based tracking
pub struct IncrementalMarkState {
    /// Current phase of incremental marking
    phase: AtomicUsize,  // 0=IDLE, 1=SNAPSHOT, 2=MARKING, 3=FINAL_MARK, 4=SWEEPING
    
    /// Work queue for objects to mark (lock-free)
    worklist: crossbeam::queue::SegQueue<NonNull<GcBox<()>>>,
    
    /// Card-based dirty record buffer (lock-free)
    dirty_records: crossbeam::queue::SegQueue<DirtyRecord>,
    
    /// Number of objects marked this increment
    marked_this_increment: AtomicUsize,
    
    /// Target: mark this many objects per increment
    increment_size: usize,
    
    /// Card-based dirty segment tracking
    dirty_segments: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
}

/// Record of a reference write during incremental marking
pub struct DirtyRecord {
    /// Location where pointer was stored (the slot)
    location: *mut *const GcBox<()>,
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

### 3.4 Write Barrier for Incremental Marking with Card Integration

#### 3.4.1 Enhanced Write Barrier

**File**: `src/cell.rs` and `src/gc/gc.rs`

The write barrier must:
1. Apply card-based generational barrier (old→young tracking)
2. Apply incremental barrier (record overwritten references)

```rust
/// Combined write barrier for card-based generational + incremental GC
fn write_barrier<T: Trace>(&self, old_value: Option<&T>, new_value: &T) {
    // 1. Card-based generational barrier (from 0.7.2)
    // If old object writing young reference, mark card dirty
    if self.is_old_generation() && new_value.is_young_generation() {
        mark_card_dirty(self.page());
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

#### 3.4.2 Dirty Record Processing

**File**: `src/gc/incremental.rs`

```rust
impl IncrementalMarkState {
    /// Called during each marking increment
    pub fn process_dirty_records(&self, budget: usize) -> usize {
        let mut processed = 0;
        
        // Process lock-free dirty records
        while processed < budget {
            if let Some(record) = self.dirty_records.pop() {
                // Mark the old value that was overwritten
                if let Some(gc_box) = unsafe { record.old_value.as_ref() } {
                    if !gc_box.is_marked() {
                        self.worklist.push(NonNull::from(gc_box));
                    }
                }
                processed += 1;
            } else {
                break;
            }
        }
        
        processed
    }
    
    /// Record a dirty reference (called from write barrier)
    pub fn record_dirty(&self, old_value: *const GcBox<()>) {
        let record = DirtyRecord {
            location: std::ptr::null_mut(), // Not needed for SATB
            old_value,
        };
        
        // Lock-free enqueue using crossbeam
        self.dirty_records.push(record);
    }
}
```

### 3.5 Incremental Mark Loop with Card Integration

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
            // Worklist empty - check dirty records
            let dirty_processed = state.process_dirty_records(budget - marked);
            marked += dirty_processed;
            
            if dirty_processed == 0 {
                // Nothing more to do
                return MarkStatus::Complete;
            }
        }
    }
    
    MarkStatus::Yield
}

/// Check if marking is complete
pub fn is_marking_complete() -> bool {
    let state = incremental_state();
    state.worklist.is_empty() && state.dirty_records.is_empty()
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
4. Add dirty record buffer
5. Create `is_incremental_marking_active()` helper
6. Unit tests for state machine

**Deliverables**:
- `src/gc/incremental.rs` - Core incremental state
- `tests/incremental_state.rs` - State machine tests

### Phase 2: Write Barrier Integration (Week 1-2)

**Tasks**:
1. Enhance write barrier to support incremental marking with card integration
2. Implement dirty record recording
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
4. Add dirty record processing
5. Mark completion detection

**Deliverables**:
- Modified `src/gc/marker.rs` - Incremental worker integration
- Modified `src/gc/gc.rs` - Incremental collection entry points
- `tests/incremental_marking.rs` - Mark loop tests

### Phase 4: Final Mark and Integration (Week 2-3)

**Tasks**:
1. Implement final mark phase (STW to complete marking)
2. Integration with card-based generational GC (0.7.2)
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
| Memory bloat (dirty records) | Medium | Medium | Bounded buffer, overflow to STW |
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

## 9. Relationship to Card-Based Generational GC

### 9.1 Integration Points

1. **Write Barrier**: Combines card-based generational and incremental checks
   ```rust
   fn write_barrier(old, new) {
       card_based_barrier(new);  // From 0.7.2
       incremental_barrier(old);   // New for 0.8
   }
   ```

2. **Collection Scheduling**:
   - Minor GC: STW, uses card-based dirty page list (from 0.7.2)
   - Major GC: Incremental, uses write barrier + dirty records

3. **Phase Coordination**:
   - Cannot run minor GC during incremental major mark
   - Must complete or abort incremental mark before minor GC

### 9.2 Configuration

```rust
pub struct GcConfig {
    // From 0.7.2
    pub generational: bool,
    pub card_size: usize,
    
    // New for 0.8
    pub incremental_marking: bool,
    pub increment_size: usize,  // Objects per increment
    pub max_dirty_records: usize,  // Buffer size before overflow
}
```

---

## 10. Comparison with Original 0.8 Plan

| Aspect | Original 0.8 Plan | Revised 0.8.1 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Mutex-protected page lists | Card-based dirty tracking |
| **Write Barrier** | Mutex contention | Lock-free atomic operations |
| **State Management** | Global state machine | Per-segment dirty tracking |
| **Complexity** | High (Mutex, snapshots, double-checks) | Low (ChezScheme patterns) |
| **Thread Safety** | Mutex + double-check patterns | Lock-free approaches |
| **Performance** | Write barrier regression | Minimal overhead |
| **Risk Level** | High | Low |

---

## 11. References

- **ChezScheme GC**: Reference for incremental marking implementation
- **V8 GC**: Inspiration for idle-time incremental marking
- **Go GC**: Reference for concurrent marking with write barriers
- **Card-Based Generational GC 0.7.2**: Prerequisite for this plan
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"

---

## 12. Open Questions

1. **Budget Size**: What increment_size provides best pause/throughput tradeoff?
2. **Dirty Record Buffer**: How large before overflow is acceptable?
3. **Idle-Time Collection**: Should we mark during idle time (like V8)?
4. **Parallel Incremental**: How to parallelize marking without losing increments?
5. **Feature Flag**: Should incremental be default-on or opt-in initially?

---

*Document generated: 2026-02-02*  
*Based on: Dybvig Review and ChezScheme Reference*  
*Prerequisite: Card-Based Generational GC 0.7.2 must be complete and stable*

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
Before mark: A -> B -> C (C is live)
During mark: Mutator changes A to point to C, drops B (B becomes garbage)
Marker visits A, marks C, never sees B
After mark: B is incorrectly collected (BUG!)
```

**Solution**: Write barrier must record mutations during incremental mark.

---

## 3. Architecture Design

### 3.1 Snapshot-At-The-Beginning (SATB)

We use the Dijkstra-style incremental update approach:

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
│                         │ Check dirty records    │          │
│                         │                        │          │
│                         ▼                        │          │
│                    Mark complete?                │          │
│                         │                        │          │
│                    Yes ─┴─ No ───────────────────┘          │
│                     │                                       │
│                     ▼                                       │
│               (STW: short)                                  │
│               Process final dirty records                   │
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
    
    /// Write barrier buffer for dirty records
    dirty_records: Mutex<Vec<DirtyRecord>>,
}

/// Record of a reference write during incremental marking
pub struct DirtyRecord {
    /// Location where pointer was stored (the slot)
    location: *mut *const GcBox<()>,
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

#### 3.4.2 Dirty Record Processing

**File**: `src/gc/incremental.rs`

```rust
impl IncrementalMarkState {
    /// Called during each marking increment
    pub fn process_dirty_records(&self, budget: usize) -> usize {
        let mut processed = 0;
        let records = self.dirty_records.lock().unwrap();
        
        for record in records.iter().take(budget) {
            // Mark the old value that was overwritten
            if let Some(gc_box) = unsafe { record.old_value.as_ref() } {
                if !gc_box.is_marked() {
                    self.worklist.push(NonNull::from(gc_box));
                }
            }
            processed += 1;
        }
        
        // Remove processed records
        drop(records);
        self.dirty_records.lock().unwrap().drain(0..processed);
        
        processed
    }
    
    /// Record a dirty reference (called from write barrier)
    pub fn record_dirty(&self, old_value: *const GcBox<()>) {
        let record = DirtyRecord {
            location: std::ptr::null_mut(), // Not needed for SATB
            old_value,
        };
        
        // Lock-free enqueue using crossbeam
        self.dirty_records.lock().unwrap().push(record);
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
            // Worklist empty - check dirty records
            let dirty_processed = state.process_dirty_records(budget - marked);
            marked += dirty_processed;
            
            if dirty_processed == 0 {
                // Nothing more to do
                return MarkStatus::Complete;
            }
        }
    }
    
    MarkStatus::Yield
}

/// Check if marking is complete
pub fn is_marking_complete() -> bool {
    let state = incremental_state();
    state.worklist.is_empty() && state.dirty_records.lock().unwrap().is_empty()
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
4. Add dirty record buffer
5. Create `is_incremental_marking_active()` helper
6. Unit tests for state machine

**Deliverables**:
- `src/gc/incremental.rs` - Core incremental state
- `tests/incremental_state.rs` - State machine tests

### Phase 2: Write Barrier Integration (Week 1-2)

**Tasks**:
1. Enhance write barrier to support incremental marking
2. Implement dirty record recording
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
4. Add dirty record processing
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
| Memory bloat (dirty records) | Medium | Medium | Bounded buffer, overflow to STW |
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

1. **Write Barrier**: Combines card-based generational and incremental checks
   ```rust
   fn write_barrier(old, new) {
       card_based_barrier(new);  // From 0.7.2
       incremental_barrier(old);   // New for 0.8
   }
   ```

2. **Collection Scheduling**:
   - Minor GC: STW, uses card-based dirty page list (from 0.7.2)
   - Major GC: Incremental, uses write barrier + dirty records

3. **Phase Coordination**:
   - Cannot run minor GC during incremental major mark
   - Must complete or abort incremental mark before minor GC

### 9.2 Configuration

```rust
pub struct GcConfig {
    // From 0.7.2
    pub generational: bool,
    pub card_size: usize,
    
    // New for 0.8
    pub incremental_marking: bool,
    pub increment_size: usize,  // Objects per increment
    pub max_dirty_records: usize,  // Buffer size before overflow
}
```

---

## 10. Comparison with Original 0.8 Plan

| Aspect | Original 0.8 Plan | Revised 0.8.1 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Mutex-protected page lists | Card-based dirty tracking |
| **Write Barrier** | Mutex contention | Lock-free atomic operations |
| **State Management** | Global state machine | Per-segment dirty tracking |
| **Complexity** | High (Mutex, snapshots, double-checks) | Low (ChezScheme patterns) |
| **Thread Safety** | Mutex + double-check patterns | Lock-free approaches |
| **Performance** | Write barrier regression | Minimal overhead |
| **Risk Level** | High | Low |

---

## 11. References

- **ChezScheme GC**: Reference for incremental marking implementation
- **V8 GC**: Inspiration for idle-time incremental marking
- **Go GC**: Reference for concurrent marking with write barriers
- **Card-Based Generational GC 0.7.2**: Prerequisite for this plan
- **Dijkstra et al.**: "On-the-Fly Garbage Collection: An Exercise in Cooperation"

---

## 12. Open Questions

1. **Budget Size**: What increment_size provides best pause/throughput tradeoff?
2. **Dirty Record Buffer**: How large before overflow is acceptable?
3. **Idle-Time Collection**: Should we mark during idle time (like V8)?
4. **Parallel Incremental**: How to parallelize marking without losing increments?
5. **Feature Flag**: Should incremental be default-on or opt-in initially?

---

*Document generated: 2026-02-02*  
*Based on: Dybvig Review and ChezScheme Reference*  
*Prerequisite: Card-Based Generational GC 0.7.2 must be complete and stable*

---

## Changelog

### Version 0.8.1 (Revised)

**Key Changes from 0.8:**

1. **Card-Based Integration**: Updated to work with the card-based dirty tracking from 0.7.2 instead of mutex-protected page lists
2. **Simplified State Management**: Reduced complexity by leveraging per-segment dirty tracking instead of global state machines
3. **Optimized Write Barrier**: Combined generational and incremental checks without mutex contention
4. **ChezScheme Alignment**: Adopted proven patterns from ChezScheme's incremental marking implementation
5. **Lock-Free Design**: Replaced Mutex-based dirty record buffers with lock-free approaches
6. **Better Performance**: Eliminated write barrier contention while maintaining correctness guarantees
