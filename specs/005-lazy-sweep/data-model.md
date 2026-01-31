# Data Model: Lazy Sweep

## Key Entities

### PageHeader

Per-page metadata structure tracking page state, including flags for sweep state and dead object count.

**Attributes**:
- `flags: u8` - Bit flags for page state (existing: PAGE_FLAG_LARGE, PAGE_FLAG_ORPHAN; new: PAGE_FLAG_NEEDS_SWEEP, PAGE_FLAG_ALL_DEAD)
- `dead_count: Cell<u16>` - Count of dead objects in this page (enables "all-dead" fast path)
- `free_list_head: Option<u16>` - Index of first free slot in page (reused from existing)
- `next_sweep_pending: Option<NonNull<PageHeader>>` - Next page needing sweep in size-class list (optional optimization)
- `prev_sweep_pending: Option<NonNull<PageHeader>>` - Previous page needing sweep in size-class list (optional optimization)

**Relationships**: One PageHeader per memory page; pages grouped by size class

**State Transitions**:

```
Allocated (has objects)
    ↓ (collection mark phase - all objects unmarked)
Mark Phase Started
    ↓ (mark phase - live objects marked)
Mark Phase Complete
    ↓ (count dead = allocated - marked)
Needs Sweep / All Dead
    ↓ (lazy sweep processes page)
Sweep Complete
    ↓ (allocation or safepoint check)
Back to Allocated
```

### Sweep State Machine

Pages transition through these states during lazy sweep:

```
┌─────────────────┐
│   ALLOCATED     │ ← New objects allocated
│  (has live objs)│
└────────┬────────┘
         │ Collection (mark phase)
         ↓
┌─────────────────┐
│ NEEDS_SWEEP     │ ← Mark complete, some objects dead
│  (has dead)     │
└────────┬────────┘
         │ Lazy sweep finds all dead
         ↓
┌─────────────────┐
│   ALL_DEAD      │ ← All objects confirmed dead
│  (fast path)    │
└────────┬────────┘
         │ Lazy sweep (fast path)
         ↓
┌─────────────────┐
│ SWEEP_COMPLETE  │ ← Free list rebuilt, flags cleared
│  (reclaimed)    │
└────────┬────────┘
         │ New allocation
         ↓
┌─────────────────┐
│   ALLOCATED     │
└─────────────────┘
```

### Free List

Per-page linked list of reclaimed objects available for allocation.

**Structure**: Implicit linked list using object headers
- Each reclaimed object stores index of next free object
- `free_list_head` in PageHeader points to first free slot

**Operations**:
- `push`: Add reclaimed object to front of list
- `pop`: Remove first free object for allocation

### LazySweepBatch

Fixed-size batch of sweep work performed during each lazy sweep operation.

**Attributes**:
- `SWEEP_BATCH_SIZE: usize = 16` - Maximum objects processed per sweep call
- `reclaimed_count: usize` - Number of objects actually reclaimed in this batch
- `found_dead: bool` - Whether any dead objects were found

**Purpose**: Bounds per-allocation overhead to prevent latency spikes

## Validation Rules

1. **dead_count** MUST equal number of allocated - marked objects after mark phase
2. **PAGE_FLAG_NEEDS_SWEEP** MUST be set if page has any allocated objects after marking
3. **PAGE_FLAG_ALL_DEAD** MUST be set only if dead_count == allocated_count
4. **PAGE_FLAG_NEEDS_SWEEP** MUST be cleared after page is fully swept
5. **PAGE_FLAG_ALL_DEAD** MUST be cleared when new allocation occurs on page

## Constraints

- Page header size MUST remain unchanged (use existing padding for dead_count)
- All operations MUST maintain O(1) allocation time
- Memory overhead: 1 byte flags + 2 bytes dead_count per page (from padding)
- Sweep batch size is fixed at 16 to bound per-allocation overhead
