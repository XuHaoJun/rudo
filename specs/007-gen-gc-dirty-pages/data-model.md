# Data Model: Generational GC Dirty Page Tracking

**Feature Branch**: `007-gen-gc-dirty-pages`  
**Date**: 2026-02-03

---

## Entity Definitions

### 1. PageHeader (Modified)

**Location**: `src/heap.rs`

**Existing Fields** (unchanged):
```rust
pub struct PageHeader {
    pub magic: u32,                               // MAGIC_GC_PAGE = 0x5255_4447
    pub block_size: u32,
    pub obj_count: u16,
    pub header_size: u16,
    pub generation: u8,                           // 0 = young, >0 = old
    pub flags: AtomicU8,                          // Existing: LARGE|ORPHAN|NEEDS_SWEEP|ALL_DEAD
    pub owner_thread: u64,
    pub dead_count: AtomicU16,
    pub mark_bitmap: [AtomicU64; BITMAP_SIZE],
    pub dirty_bitmap: [AtomicU64; BITMAP_SIZE],   // Per-object dirty tracking (kept)
    pub allocated_bitmap: [AtomicU64; BITMAP_SIZE],
    pub free_list_head: AtomicU16,
}
```

**New Flag Constant**:
```rust
pub const PAGE_FLAG_DIRTY_LISTED: u8 = 0x10;  // Page is in dirty_pages list
```

**Validation Rules**:
- `PAGE_FLAG_DIRTY_LISTED` must be set only when page is in `dirty_pages` Vec
- Flag must be cleared after page is processed in minor GC
- Only old-generation pages (generation > 0) should have this flag set

### 2. LocalHeap (Modified)

**Location**: `src/heap.rs`

**Existing Fields** (unchanged):
```rust
pub struct LocalHeap {
    pub tlab_16: Tlab,
    // ... other TLABs ...
    pub pages: Vec<NonNull<PageHeader>>,
    pub small_pages: HashSet<usize>,
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,
    young_allocated: usize,
    old_allocated: usize,
    min_addr: usize,
    max_addr: usize,
}
```

**New Fields**:
```rust
/// Mutex-protected list of pages with dirty objects (old generation only)
/// Cleared at the end of each minor GC cycle
dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,

/// Snapshot taken at GC start for lock-free scanning
/// Populated from dirty_pages, cleared after use
dirty_pages_snapshot: Vec<NonNull<PageHeader>>,

/// Rolling average of dirty pages per GC cycle (for capacity planning)
avg_dirty_pages: usize,

/// History of dirty page counts (last 4 cycles)
dirty_page_history: [usize; 4],
```

**Invariants**:
- `dirty_pages` contains only old-generation pages
- Each page appears at most once in `dirty_pages` (enforced by `PAGE_FLAG_DIRTY_LISTED`)
- `dirty_pages_snapshot` is empty outside of GC
- `dirty_pages` may grow during GC (new mutations) - these are handled in next cycle

### 3. GcVisitor (Unchanged)

**Location**: `src/gc/gc.rs`

No changes required. `VisitorKind::Minor` filtering continues to work as-is.

---

## State Transitions

### Page Lifecycle with Dirty Tracking

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         PAGE STATE MACHINE                               │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  ┌──────────┐                                                           │
│  │ YOUNG    │ ──(survive GC)──> ┌────────────────┐                      │
│  │ gen=0    │                   │ OLD (CLEAN)    │                      │
│  │ flag=0   │                   │ gen=1, flag=0  │                      │
│  └──────────┘                   └────────────────┘                      │
│       ↑                                  │                              │
│       │                                  │ (write barrier fires)        │
│    allocate                              ↓                              │
│                                 ┌────────────────────┐                  │
│                                 │ OLD (DIRTY)        │                  │
│                                 │ gen=1              │                  │
│                                 │ flag=DIRTY_LISTED  │                  │
│                                 │ in dirty_pages[]   │                  │
│                                 └────────────────────┘                  │
│                                          │                              │
│                                          │ (minor GC: take snapshot)    │
│                                          ↓                              │
│                                 ┌────────────────────┐                  │
│                                 │ OLD (SCANNING)     │                  │
│                                 │ in snapshot[]      │                  │
│                                 │ scan dirty objects │                  │
│                                 └────────────────────┘                  │
│                                          │                              │
│                                          │ (clear dirty bits & flag)    │
│                                          ↓                              │
│                                 ┌────────────────┐                      │
│                                 │ OLD (CLEAN)    │ ←──────────────────┐ │
│                                 │ gen=1, flag=0  │                    │ │
│                                 └────────────────┘                    │ │
│                                          │                            │ │
│                                          └────(no mutations)──────────┘ │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Dirty Page List Lifecycle

```
┌─────────────────────────────────────────────────────────────────────────┐
│                      DIRTY PAGE LIST LIFECYCLE                          │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  MUTATOR PHASE (between GCs):                                           │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │ For each old-gen mutation:                                      │    │
│  │   1. Set dirty bit (atomic, lock-free)                          │    │
│  │   2. Check PAGE_FLAG_DIRTY_LISTED                                │    │
│  │   3. If not set: acquire mutex, double-check, add to list       │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                                                         │
│  GC START:                                                              │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │   1. Acquire dirty_pages mutex                                   │    │
│  │   2. Move dirty_pages → dirty_pages_snapshot (drain)            │    │
│  │   3. Release mutex                                               │    │
│  │   4. Mutators can now add new pages to dirty_pages              │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                                                         │
│  GC SCAN (lock-free):                                                   │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │ For each page in snapshot:                                       │    │
│  │   1. Scan dirty objects (is_dirty check)                        │    │
│  │   2. Trace young references                                     │    │
│  │   3. Clear dirty bits (clear_all_dirty)                         │    │
│  │   4. Clear PAGE_FLAG_DIRTY_LISTED flag                          │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                                                         │
│  GC END:                                                                │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │   1. Clear snapshot                                              │    │
│  │   2. Update statistics (avg_dirty_pages)                        │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Memory Layout

### PageHeader Flag Bits

```
┌───────────────────────────────────────────────────────────────┐
│                    PageHeader.flags (AtomicU8)                │
├───────────────────────────────────────────────────────────────┤
│  Bit 7   Bit 6   Bit 5   Bit 4   Bit 3   Bit 2   Bit 1   Bit 0│
│  ────    ────    ────    ────    ────    ────    ────    ──── │
│ unused  unused  unused  DIRTY   ALL     NEEDS   ORPHAN  LARGE │
│                         LISTED  DEAD    SWEEP                 │
│                         (NEW)   (lazy)  (lazy)                │
└───────────────────────────────────────────────────────────────┘
```

### LocalHeap Memory Impact

| Field | Size | Notes |
|-------|------|-------|
| `dirty_pages` | 48 bytes | Mutex + Vec (pointer + len + cap) |
| `dirty_pages_snapshot` | 24 bytes | Vec (pointer + len + cap) |
| `avg_dirty_pages` | 8 bytes | usize |
| `dirty_page_history` | 32 bytes | [usize; 4] |
| **Total overhead** | **112 bytes** | Per LocalHeap (per thread) |

---

## Relationships

```
┌─────────────┐         ┌──────────────────┐
│  LocalHeap  │ 1───* │  PageHeader      │
├─────────────┤         ├──────────────────┤
│ pages       │────────>│ generation       │
│ dirty_pages │────────>│ flags            │
│ snapshot    │         │ dirty_bitmap     │
└─────────────┘         └──────────────────┘
        │                       │
        │                       │ (contains)
        │                       ↓
        │               ┌──────────────────┐
        │               │  GcBox<T>        │
        │               ├──────────────────┤
        └───────────────│ (allocated in    │
                        │  page's data     │
                        │  region)         │
                        └──────────────────┘
                                │
                                │ (wraps)
                                ↓
                        ┌──────────────────┐
                        │  GcCell<T>       │
                        ├──────────────────┤
                        │ write_barrier()  │───> sets dirty bit
                        │                  │───> adds page to list
                        └──────────────────┘
```

---

## Concurrency Considerations

### Lock Ordering (Extended)

```
1. LocalHeap (order 1) - per-thread, no lock needed
2. dirty_pages Mutex (order 1.5) - NEW, within LocalHeap
3. GlobalMarkState (order 2)
4. GC Request (order 3)
```

The `dirty_pages` mutex is acquired and released within write barrier execution, never held across other lock acquisitions.

### Atomic Operations Summary

| Operation | Ordering | Justification |
|-----------|----------|---------------|
| Check `PAGE_FLAG_DIRTY_LISTED` | Acquire | See updates from other threads |
| Set `PAGE_FLAG_DIRTY_LISTED` | Release | Publish Vec push |
| Clear `PAGE_FLAG_DIRTY_LISTED` | Release | Publish for next cycle |
| Flag read inside mutex | Relaxed | Mutex provides synchronization |
