# Data Model: Parallel Marking for rudo-gc

**Feature**: Parallel Marking  
**Date**: 2026-01-27

---

## Entities

### 1. StealQueue<T, const N: usize>

Lock-free work-stealing queue based on Chase-Lev algorithm.

**Fields**:

| Field | Type | Invariant |
|-------|------|-----------|
| `buffer` | `[MaybeUninit<T>; N]` | N must be power of 2 |
| `bottom` | `Cell<usize>` | Producer pointer |
| `top` | `AtomicUsize` | Consumer pointer (atomic) |
| `mask` | `usize` | `N - 1` |

**Operations**:

| Operation | Complexity | Description |
|-----------|------------|-------------|
| `push(bottom, item)` | O(1) amortized | LIFO push (producer end) |
| `pop(bottom)` | O(1) amortized | LIFO pop (producer end) |
| `steal()` | O(1) amortized | FIFO steal (consumer end) |
| `len(bottom)` | O(1) | Current size |

**Invariants**:
- Queue empty when `bottom == top`
- Queue full when `bottom - top == N`
- Size always `bottom - top` (modulo arithmetic)

---

### 2. PerThreadMarkQueue

Per-thread mark queue with local and steal components.

**Fields**:

| Field | Type | Description |
|-------|------|-------------|
| `local_queue` | `Worklist<1024>` | Local work queue (LIFO) |
| `steal_queue` | `StealQueue<NonNull<GcBox<()>>, 1024>` | Stealable queue (FIFO) |
| `owned_pages` | `Vec<NonNull<PageHeader>>` | Pages owned by this thread |
| `marked_count` | `AtomicUsize>` | Atomic count of marked objects |
| `thread_id` | `ThreadId` | Thread identifier |

**Operations**:

| Operation | Description |
|-----------|-------------|
| `push_local(ptr)` | Push to local queue (LIFO, fastest path) |
| `pop_local()` | Pop from local queue |
| `steal()` | Steal from this queue (called by other threads) |
| `process_owned_page(page)` | Process all objects on an owned page |
| `marked_count()` | Get marked count |

---

### 3. ParallelMarkCoordinator

Coordinates parallel marking across multiple workers.

**Fields**:

| Field | Type | Description |
|-------|------|-------------|
| `queues` | `Vec<PerThreadMarkQueue>` | Per-thread mark queues |
| `barrier` | `Barrier` | Synchronization barrier |
| `page_to_queue` | `HashMap<usize, usize>` | Page address -> queue index mapping |
| `total_marked` | `AtomicUsize>` | Total marked count |

**Operations**:

| Operation | Description |
|-----------|-------------|
| `new(num_workers)` | Create new coordinator |
| `register_pages(queue_idx, pages)` | Register pages for a queue |
| `distribute_roots(roots, find_gc_box)` | Distribute roots to appropriate queues |
| `mark(heap, kind)` | Execute parallel marking |

---

### 4. PageHeader Modifications

Added fields and methods to support parallel marking.

**Added Fields**:

| Field | Type | Description |
|-------|------|-------------|
| `owner_thread` | `ThreadId` | Thread that allocated this page |

**Added Methods**:

| Method | Returns | Description |
|--------|---------|-------------|
| `try_mark(index)` | `bool` | Atomically try to mark (CAS-based) |
| `is_fully_marked()` | `bool` | Check if all objects are marked |

**Validation Rules**:
- `try_mark()` returns `false` if already marked
- `try_mark()` returns `true` if newly marked
- `is_fully_marked()` checks all bits in mark bitmap

---

## Relationships

```
ParallelMarkCoordinator
├── 1:N -> PerThreadMarkQueue (one per worker)
└── 1:N -> HashMap<page_addr, queue_idx>

PerThreadMarkQueue
├── 1:N -> PageHeader (owned_pages)
└── 1:1 -> Worklist (local_queue)
    └── 1:1 -> StealQueue (steal_queue)

StealQueue<T, N>
└── N:T -> T (buffer array)
```

---

## State Transitions

### PerThreadMarkQueue State

```
IDLE -> PROCESSING_OWNED -> PROCESSING_LOCAL -> STEALING -> IDLE
                    ↑                      |
                    |______________________|
                    (continue if work found)
```

### ParallelMarkCoordinator State

```
CREATED -> REGISTERING_PAGES -> DISTRIBUTING_ROOTS -> MARKING -> DONE
```

---

## Validation Rules

### StealQueue Invariants (enforced by implementation)

1. `N` must be a power of 2 (checked in `new()`)
2. `mask = N - 1` (computed in `new()`)
3. Push only when `bottom - top < N` (check before push)
4. Pop only when `bottom != top` (check before pop)
5. Steal only when `bottom != top` (check before steal)

### PageHeader Invariants

1. `owner_thread` set at page allocation time
2. `try_mark()` uses CAS to prevent race conditions
3. `is_fully_marked()` checks all objects in page

---

## Memory Layout

```
┌─────────────────────────────────────────┐
│         StealQueue<T, 1024>              │
├─────────────────────────────────────────┤
│ buffer: [MaybeUninit<T>; 1024]           │ 8192 bytes (for T = u8)
│ bottom: Cell<usize>                      │ 8 bytes
│ top: AtomicUsize                         │ 8 bytes
│ mask: usize                              │ 8 bytes
└─────────────────────────────────────────┘

┌─────────────────────────────────────────┐
│        PerThreadMarkQueue                │
├─────────────────────────────────────────┤
│ local_queue: Worklist<1024>              │ ~8192 bytes
│ steal_queue: StealQueue<NonNull<...>>    │ ~8224 bytes
│ owned_pages: Vec<NonNull<PageHeader>>    │ 24 bytes (empty vec)
│ marked_count: AtomicUsize                │ 8 bytes
│ thread_id: ThreadId                      │ 8 bytes
└─────────────────────────────────────────┘

┌─────────────────────────────────────────┐
│       ParallelMarkCoordinator            │
├─────────────────────────────────────────┤
│ queues: Vec<PerThreadMarkQueue>          │ N * ~16448 bytes
│ barrier: Barrier                         │ ~48 bytes
│ page_to_queue: HashMap<usize, usize>     │ variable
│ total_marked: AtomicUsize                │ 8 bytes
└─────────────────────────────────────────┘
```
