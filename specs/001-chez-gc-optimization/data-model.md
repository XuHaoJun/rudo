# Data Model: Chez Scheme GC optimizations for rudo-gc

## Entities

### PerThreadMarkQueue

Thread-local work queue that holds marking work items to be processed.

**Fields**:
- `queue: StealQueue<MarkWork>` - Chase-Lev work-stealing deque for local work
- `pending_work: Mutex<Vec<MarkWork>>` - Work received from other threads via push-based transfer
- `work_available: Notify` - Notification mechanism for workers waiting on pending work
- `owned_pages: HashSet<PagePtr>` - Set of pages owned by this thread for ownership-based distribution
- `capacity_hint: AtomicUsize` - Target capacity for dynamic stack growth

**Relationships**:
- Multiple instances managed by `GlobalMarkState`
- Each queue is owned by exactly one worker thread
- Queues interact via work stealing and push-based transfer

**State Transitions**:
- `Idle` -> `Working` when work added to local queue
- `Working` -> `Stealing` when local queue empty
- `Stealing` -> `Receiving` when notified of pending work
- `Receiving` -> `Working` when pending work drained

---

### MarkBitmap

Page-level structure that records which objects have been marked.

**Fields**:
- `bitmap: Vec<u64>` - Bitmap storage, one bit per pointer-sized unit
- `capacity: usize` - Number of pointer slots in the page
- `marked_count: AtomicUsize` - Number of marked slots (atomic for parallel access)

**Relationships**:
- Owned by `PageHeader`
- One bitmap per page when in mark-bitmap mode
- Read by sweep phase to determine object liveness

**Validation Rules**:
- `capacity` must be aligned to 64 (pointer slots per u64 word)
- `bitmap.len()` must equal `(capacity + 63) / 64`
- Mark operations are idempotent (setting already-set bit is no-op)

**State Transitions**:
- `Empty` -> `Marking` when mark phase begins
- `Marking` -> `Complete` when mark phase ends
- `Complete` -> `Empty` when sweep phase clears for reuse

---

### PageHeader

Metadata structure for each heap page.

**Fields**:
- `owner_thread: ThreadId` - Thread that allocated this page
- `bitmap: Option<MarkBitmap>` - Mark bitmap (None if using forwarding pointers)
- `generation: u8` - Generation number for generational GC
- `space: u8` - Space type (young, old, immutable, etc.)

**Relationships**:
- One header per heap page
- Referenced by `PerThreadMarkQueue.owned_pages`
- Used by `GlobalMarkState` for work distribution

---

### GlobalMarkState

Coordinator that manages all worker queues and tracks overall mark phase progress.

**Fields**:
- `queues: Vec<Arc<PerThreadMarkQueue>>` - All worker queues
- `mark_bitmap: PageBitmap` - Bitmap for the current mark phase
- `work_completed: AtomicUsize` - Count of completed work items
- `phase: AtomicU8` - Current phase (Idle, Marking, Sweeping)

**Relationships**:
- Singleton coordinating all `PerThreadMarkQueue` instances
- Owns `PageHeader` metadata
- Coordinates mark phase lifecycle

---

### LockOrderingDiscipline

Rules specifying valid lock acquisition order.

**Fields** (documentation only):
- `ORDER_LOCAL_HEAP: u8 = 1` - LocalHeap lock has order 1
- `ORDER_GLOBAL_MARK: u8 = 2` - GlobalMarkState lock has order 2
- `ORDER_GC_REQUEST: u8 = 3` - GC Request lock has order 3

**Validation**:
- Lock tags checked in debug builds
- Violations cause panic with ordering violation message
- Production builds skip check for performance

---

## Data Flow

### Mark Phase Flow

```
1. GC trigger -> GlobalMarkState.phase = Marking
2. Roots pushed to local PerThreadMarkQueue.queue
3. Workers poll local queue (LIFO) and steal (FIFO)
4. When worker encounters remote reference:
   a. Push to owner's PerThreadMarkQueue.pending_work
   b. Notify owner via work_available
5. Owner drains pending_work when local queue empty
6. Mark operation sets bit in PageHeader.bitmap
7. When queue empty, worker calls receive_work()
8. All workers complete when work_completed = total_work
9. GlobalMarkState.phase = Sweeping
10. Sweep phase reads bitmap to determine liveness
```

### Ownership-Based Distribution Flow

```
1. Allocation records owner_thread in PageHeader
2. Page added to owner's PerThreadMarkQueue.owned_pages
3. When marking owned page, work pushed to local queue
4. When marking remote page, work pushed to owner's pending_work
5. Stealing prioritizes queues of page owners (get_owned_queues)
```

---

## Key Invariants

1. Each page has exactly one owner_thread
2. Each owned_pages entry maps to exactly one PerThreadMarkQueue
3. Mark bitmap bits are only set, never cleared during marking
4. Lock acquisition follows LocalHeap -> GlobalMarkState -> GC Request order
5. No two workers mark the same object concurrently (work queue ensures serial access)

---

## Migration from Forwarding Pointers

**Transition**:
- Remove `forwarding: GcHeader` field from `GcBox<T>`
- Add `bitmap: Option<MarkBitmap>` to `PageHeader`
- Mark phase: `bitmap.mark(slot_index)` instead of `box.forwarding = header`
- Sweep phase: `bitmap.is_marked(slot_index)` instead of `box.forwarding != 0`

**Validation**:
- All objects must have their mark bit set during migration
- Backward compatibility mode provided during transition
- One-time migration with verification tests
