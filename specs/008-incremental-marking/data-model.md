# Data Model: Incremental Marking for Major GC

**Feature**: 008-incremental-marking  
**Date**: 2026-02-03  
**Status**: Complete

---

## 1. State Machine

### 1.1 Mark Phase States

```rust
/// Phases of incremental marking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum MarkPhase {
    /// No collection in progress. Mutators run freely.
    Idle = 0,
    
    /// STW: Capturing roots, clearing marks, initializing worklist.
    /// Duration: Short (proportional to root set size)
    Snapshot = 1,
    
    /// Incremental marking in progress. Mutators interleave with mark slices.
    /// Write barrier active (SATB + Dijkstra).
    Marking = 2,
    
    /// STW: Final scan of dirty pages, verify marking complete.
    /// Duration: Short (proportional to remaining dirty pages)
    FinalMark = 3,
    
    /// Sweep phase (can be lazy). Mutators run.
    Sweeping = 4,
}
```

### 1.2 State Transitions

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        Incremental Marking FSM                          │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  ┌──────┐    trigger_major_gc()    ┌──────────┐                        │
│  │ Idle │ ─────────────────────────> │ Snapshot │                        │
│  └──────┘                           └──────────┘                        │
│      ^                                   │                              │
│      │                                   │ roots_captured()             │
│      │                                   v                              │
│      │                              ┌─────────┐                         │
│      │                              │ Marking │ <────────┐              │
│      │                              └─────────┘          │              │
│      │                                   │               │              │
│      │             mark_complete() ──────┤               │              │
│      │                                   │   slice_done() && !empty     │
│      │                                   v               │              │
│      │                             ┌───────────┐         │              │
│      │                             │ FinalMark │ ────────┘              │
│      │                             └───────────┘  (more_work)           │
│      │                                   │                              │
│      │                                   │ all_marked()                 │
│      │                                   v                              │
│      │                              ┌──────────┐                        │
│      └───────────────────────────── │ Sweeping │                        │
│               sweep_done()          └──────────┘                        │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### 1.3 Transition Table

| From | Event | To | Condition | Action |
|------|-------|-----|-----------|--------|
| Idle | `trigger_major_gc()` | Snapshot | Major GC requested | Stop mutators |
| Snapshot | `roots_captured()` | Marking | Roots in worklist | Resume mutators, enable write barrier |
| Marking | `slice_done()` | Marking | Worklist or dirty snapshot not empty | Continue next slice |
| Marking | `mark_complete()` | FinalMark | Worklist empty, dirty snapshot drained | Stop mutators for final scan |
| Marking | `fallback_triggered()` | FinalMark | Threshold exceeded | Stop mutators, complete STW |
| FinalMark | `all_marked()` | Sweeping | No remaining work | Begin sweep, resume mutators |
| FinalMark | `more_work()` | Marking | Dirty pages found during final | Resume mutators, continue marking |
| Sweeping | `sweep_done()` | Idle | All pages swept | Collection complete |

---

## 2. Core Entities

### 2.1 IncrementalMarkState

Global singleton managing incremental marking coordination.

```rust
/// Global incremental marking state
pub struct IncrementalMarkState {
    /// Current phase (atomic for lock-free reads)
    phase: AtomicUsize,
    
    /// Lock-free worklist using crossbeam SegQueue
    /// Objects pending marking
    worklist: crossbeam::queue::SegQueue<NonNull<GcBox<()>>>,
    
    /// Configuration for incremental behavior
    config: IncrementalConfig,
    
    /// Statistics for current mark cycle
    stats: MarkStats,
    
    /// Fallback flag: if set, complete marking STW
    fallback_requested: AtomicBool,
}

/// Configuration options
pub struct IncrementalConfig {
    /// Objects to mark per increment (default: 1000)
    pub increment_size: usize,
    
    /// Max dirty pages before fallback (default: 1000)
    pub max_dirty_pages: usize,
    
    /// Per-thread remembered buffer size (default: 32)
    pub remembered_buffer_len: usize,
    
    /// Slice timeout in milliseconds (default: 50)
    pub slice_timeout_ms: u64,
    
    /// Enable incremental marking (default: false)
    pub enabled: bool,
}

/// Statistics for monitoring
pub struct MarkStats {
    /// Objects marked this cycle
    pub objects_marked: AtomicUsize,
    
    /// Dirty pages processed
    pub dirty_pages_scanned: AtomicUsize,
    
    /// Number of mark slices executed
    pub slices_executed: AtomicUsize,
    
    /// Time spent in marking (nanoseconds)
    pub mark_time_ns: AtomicU64,
    
    /// Whether fallback to STW occurred
    pub fallback_occurred: AtomicBool,
}
```

### 2.2 ThreadControlBlock Extensions

Per-thread state for incremental marking.

```rust
pub struct ThreadControlBlock {
    // ... existing fields from spec 007 ...
    
    /// Local work queue for incremental marking
    /// Reduces contention on global worklist
    local_mark_queue: Vec<NonNull<GcBox<()>>>,
    
    /// Number of objects this thread marked this slice
    marked_this_slice: usize,
    
    /// Per-thread remembered buffer for write barrier batching
    /// Holds dirty pages before flushing to global list
    remembered_buffer: Vec<NonNull<PageHeader>>,
    
    /// Flag indicating if work-stealing is allowed for current slice
    /// Set to false at slice boundaries to prevent slice drift
    stealing_allowed: bool,
}
```

### 2.3 PageHeader Extensions

Page metadata for incremental marking.

```rust
pub struct PageHeader {
    // ... existing fields ...
    
    /// Mark bitmap for this page (existing, used by incremental)
    mark_bits: [AtomicU64; BITMAP_SIZE],
    
    /// Dirty flag for generational GC (existing from spec 007)
    dirty_bits: [AtomicU64; BITMAP_SIZE],
    
    /// In dirty list flag (existing from spec 007)
    in_dirty_list: AtomicBool,
}
```

### 2.4 GcRequest Extensions

GC request with incremental option.

```rust
pub struct GcRequest {
    pub collection_type: CollectionType,
    pub priority: GcPriority,
}

pub enum CollectionType {
    Minor,
    Major,
    /// Major collection with incremental marking
    IncrementalMajor,
}
```

---

## 3. Write Barrier Record

Records mutations during incremental marking for SATB correctness.

```rust
/// Record of a reference overwrite during incremental marking
/// Used for SATB (Snapshot-At-The-Beginning) correctness
pub struct DirtyRecord {
    /// The page containing the overwritten slot
    page: NonNull<PageHeader>,
    
    /// Object index within the page
    object_index: usize,
}
```

Note: In our implementation, we use page-level granularity (dirty page list) rather than per-slot recording. This simplifies the barrier but may cause some unnecessary re-scanning.

---

## 4. Mark Slice Result

Result of executing one incremental mark slice.

```rust
/// Result of a mark slice execution
pub enum MarkSliceResult {
    /// Slice completed, more work remains
    Pending {
        objects_marked: usize,
        dirty_pages_remaining: usize,
    },
    
    /// All marking work complete
    Complete {
        total_objects_marked: usize,
        total_slices: usize,
    },
    
    /// Fallback triggered, switching to STW
    Fallback {
        reason: FallbackReason,
    },
}

pub enum FallbackReason {
    DirtyPagesExceeded,
    SliceTimeout,
    WorklistUnbounded,
}
```

---

## 5. Validation Rules

### 5.1 State Invariants

| Invariant | Description |
|-----------|-------------|
| `phase ∈ {0,1,2,3,4}` | Phase is valid enum value |
| `Snapshot → worklist.is_empty()` initially | Worklist empty before root capture |
| `Marking → write_barrier_active` | Write barrier must be active during marking |
| `FinalMark → mutators_stopped` | Final mark is STW |
| `Sweeping → worklist.is_empty()` | No pending mark work during sweep |

### 5.2 Transition Preconditions

| Transition | Precondition |
|------------|--------------|
| `Idle → Snapshot` | No active collection |
| `Snapshot → Marking` | Roots captured, worklist populated |
| `Marking → FinalMark` | Worklist empty OR fallback triggered |
| `FinalMark → Sweeping` | All reachable objects marked |
| `Sweeping → Idle` | Sweep complete |

### 5.3 Correctness Properties

1. **No Lost Objects**: Any object reachable at snapshot time OR becoming reachable during marking is preserved
2. **Termination**: Marking terminates (via completion or fallback)
3. **Bounded Pauses**: STW pauses (Snapshot, FinalMark) are bounded
4. **Memory Safety**: No use-after-free, no double-free

---

## 6. Relationships

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          Entity Relationships                            │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌─────────────────────┐         ┌──────────────────────┐              │
│  │ IncrementalMarkState│◄────────│   ThreadControlBlock │              │
│  │  (global singleton) │ 1    N  │    (per-thread)      │              │
│  └─────────────────────┘         └──────────────────────┘              │
│           │                               │                             │
│           │ owns                          │ owns                        │
│           ▼                               ▼                             │
│  ┌─────────────────────┐         ┌──────────────────────┐              │
│  │      Worklist       │         │   local_mark_queue   │              │
│  │ (lock-free SegQueue)│         │      (Vec)           │              │
│  └─────────────────────┘         └──────────────────────┘              │
│           │                               │                             │
│           │ contains                      │ contains                    │
│           ▼                               ▼                             │
│  ┌─────────────────────────────────────────────────────────────────┐  │
│  │                         GcBox<T>                                 │  │
│  │                    (objects to mark)                             │  │
│  └─────────────────────────────────────────────────────────────────┘  │
│                                                                          │
│  ┌─────────────────────┐         ┌──────────────────────┐              │
│  │     LocalHeap       │ 1    N  │     PageHeader       │              │
│  │   (per-thread)      │◄────────│    (per-page)        │              │
│  └─────────────────────┘         └──────────────────────┘              │
│           │                               │                             │
│           │ owns                          │ contains                    │
│           ▼                               ▼                             │
│  ┌─────────────────────┐         ┌──────────────────────┐              │
│  │    dirty_pages      │         │     mark_bits        │              │
│  │ (mutex-protected)   │         │  (atomic bitmap)     │              │
│  └─────────────────────┘         └──────────────────────┘              │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

*Generated by /speckit.plan | 2026-02-03*
