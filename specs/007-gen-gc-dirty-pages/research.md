# Research: Generational GC Dirty Page Tracking

**Feature Branch**: `007-gen-gc-dirty-pages`  
**Date**: 2026-02-03  
**Status**: Complete

## Executive Summary

This research documents the current rudo-gc implementation and Chez Scheme's approach to dirty tracking for generational GC. The goal is to optimize minor collection from O(num_pages) to O(dirty_pages).

---

## 1. Current rudo-gc Implementation Analysis

### 1.1 PageHeader Structure

**Location**: `crates/rudo-gc/src/heap.rs:525-563`

```rust
#[repr(C)]
pub struct PageHeader {
    pub magic: u32,                               // MAGIC_GC_PAGE = 0x5255_4447
    pub block_size: u32,                          // Size of each object slot
    pub obj_count: u16,                           // Maximum objects per page
    pub header_size: u16,                         // Offset to first object
    pub generation: u8,                           // 0 = young, >0 = old
    pub flags: AtomicU8,                          // LARGE, ORPHAN, NEEDS_SWEEP, ALL_DEAD
    pub owner_thread: u64,                        // Thread ID for work-stealing
    pub dead_count: AtomicU16,                    // Count of dead objects (lazy-sweep)
    pub mark_bitmap: [AtomicU64; BITMAP_SIZE],    // 64 words = 4096 bits
    pub dirty_bitmap: [AtomicU64; BITMAP_SIZE],   // Per-object dirty tracking
    pub allocated_bitmap: [AtomicU64; BITMAP_SIZE], // Allocation tracking
    pub free_list_head: AtomicU16,                // Free list (lazy-sweep)
}
```

**Key Observations**:
- `generation` field exists (0=young, >0=old) - ready for use
- `flags` is `AtomicU8` with bits: LARGE(0x01), ORPHAN(0x02), NEEDS_SWEEP(0x04), ALL_DEAD(0x08)
- **Available flag bit for DIRTY_LISTED: 0x10** (currently unused)
- `dirty_bitmap` already provides per-object dirty tracking
- `BITMAP_SIZE = 64` supports up to 4096 objects per page

### 1.2 LocalHeap Structure

**Location**: `crates/rudo-gc/src/heap.rs:1177-1235`

```rust
pub struct LocalHeap {
    // TLABs for size classes 16, 32, 64, 128, 256, 512, 1024, 2048
    pub tlab_16: Tlab,
    // ... other TLABs ...
    
    pub pages: Vec<NonNull<PageHeader>>,           // All pages
    pub small_pages: HashSet<usize>,               // O(1) validation
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,
    
    young_allocated: usize,
    old_allocated: usize,
    min_addr: usize,
    max_addr: usize,
}
```

**Key Observations**:
- `pages` contains all pages (young + old, small + large)
- No existing dirty page list - this is what we need to add
- Thread-local access via TLS (no locking for heap access)

### 1.3 Current Write Barrier

**Location**: `crates/rudo-gc/src/cell.rs:76-125`

```rust
fn write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    unsafe {
        crate::heap::with_heap(|heap| {
            let page_addr = (ptr as usize) & crate::heap::page_mask();
            let is_large = heap.large_object_map.contains_key(&page_addr);
            
            if is_large {
                // Large object path - check generation, set dirty bit
                if (*header).generation > 0 {
                    (*header).set_dirty(index);
                }
            } else {
                // Small object path - check generation, set dirty bit
                if (*header.as_ptr()).generation > 0 {
                    (*header.as_ptr()).set_dirty(index);
                }
            }
        });
    }
}
```

**Performance Characteristics**:
- Lock-free using atomic operations
- Early exit for young generation (generation == 0)
- O(1) page lookup via pointer masking
- `set_dirty()` uses `fetch_or` with `AcqRel` ordering
- Cost: ~10 operations + 1 atomic operation per barrier

**Key Insight**: Write barrier only sets the dirty bit - it does NOT add page to any list. This is the integration point for dirty page tracking.

### 1.4 Current Minor Collection (The Bottleneck)

**Location**: `crates/rudo-gc/src/gc/gc.rs:1213-1266`

```rust
fn mark_minor_roots(heap: &LocalHeap) {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);

    // 1. Mark stack roots
    // ... (unchanged) ...

    // 2. PROBLEM: Iterate ALL pages
    for page_ptr in heap.all_pages() {  // ← O(num_pages)
        unsafe {
            let header = page_ptr.as_ptr();
            if (*header).generation == 0 { continue; }  // Skip young
            
            // For old pages, scan dirty objects
            for i in 0..obj_count {
                if (*header).is_dirty(i) {  // ← O(dirty_objects) - this is fine
                    ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
                }
            }
            (*header).clear_all_dirty();
        }
    }
}
```

**Complexity Analysis**:
- Current: O(num_pages) + O(dirty_objects)
- Target: O(dirty_pages) + O(dirty_objects)
- With 10,000 pages and 10 dirty pages: 1000x improvement in page iteration

### 1.5 Dirty Bitmap Operations

**Location**: `crates/rudo-gc/src/heap.rs:669-697`

```rust
pub fn is_dirty(&self, index: usize) -> bool {
    let word = index / 64;
    let bit = index % 64;
    (self.dirty_bitmap[word].load(Ordering::Acquire) & (1 << bit)) != 0
}

pub fn set_dirty(&mut self, index: usize) {
    let word = index / 64;
    let bit = index % 64;
    self.dirty_bitmap[word].fetch_or(1u64 << bit, Ordering::AcqRel);
}

pub fn clear_all_dirty(&mut self) {
    for word in &self.dirty_bitmap {
        word.store(0, Ordering::Release);
    }
}
```

**Key Points**:
- Per-object granularity (not per-page)
- Atomic operations with proper ordering
- Existing functionality preserved

---

## 2. Chez Scheme Implementation Analysis

### 2.1 Card Table Structure

Chez Scheme uses a hybrid card table + dirty list design:

```c
// Per-segment dirty tracking
octet dirty_bytes[cards_per_segment];  // One byte per card (512-byte cards)
octet min_dirty_byte;                   // Minimum dirty byte (youngest gen)

// Dirty lists organized by generation pairs
seginfo *dirty_next;     // Next in dirty list
seginfo **dirty_prev;    // Prev pointer (for O(1) removal)
```

**Key Insight**: Chez stores the youngest generation referenced in each card byte, not just a dirty flag. This enables generation-aware scanning.

### 2.2 Write Barrier (S_dirty_set)

```c
void S_dirty_set(ptr *loc, ptr x) {
    *loc = x;  // Perform write first
    if (!Sfixnump(x)) {
        seginfo *si = SegInfo(addr_get_segment(TO_PTR(loc)));
        if (si->generation != 0) {  // Only for old generation
            alloc_mutex_acquire();
            si->dirty_bytes[cardno] = 0;  // Mark card dirty
            mark_segment_dirty(si, from_g, 0);  // Add to dirty list
            alloc_mutex_release();
        }
    }
}
```

**Key Observations**:
- Mutex-protected (not lock-free)
- `mark_segment_dirty` adds segment to dirty list only if not already present
- Uses `min_dirty_byte` to track if segment is in any dirty list

### 2.3 Dirty List Management

```c
FORCEINLINE void mark_segment_dirty(seginfo *si, IGEN from_g, IGEN to_g) {
    IGEN old_to_g = si->min_dirty_byte;
    if (to_g < old_to_g) {
        // Remove from old list if present
        if (old_to_g != 0xff) {
            seginfo *next = si->dirty_next, **prev = si->dirty_prev;
            *prev = next;
            if (next != NULL) next->dirty_prev = prev;
        }
        // Add to new list
        *pointer_to_first = si;
        si->dirty_prev = pointer_to_first;
        si->dirty_next = oldfirst;
        si->min_dirty_byte = to_g;
    }
}
```

**Key Insight**: Uses `min_dirty_byte != 0xff` as the "in-list" flag, similar to our proposed `PAGE_FLAG_DIRTY_LISTED`.

### 2.4 Tradeoffs Made

| Aspect | Chez Scheme | rudo-gc Current | rudo-gc Proposed |
|--------|-------------|-----------------|------------------|
| Granularity | Per-card (512 bytes) | Per-object | Per-object (keep) |
| List Structure | Doubly-linked | N/A | Vec with snapshot |
| In-List Flag | `min_dirty_byte != 0xff` | N/A | `PAGE_FLAG_DIRTY_LISTED` |
| Synchronization | Mutex | N/A | Mutex (parking_lot) |
| Generation Tracking | Per-card byte | Binary dirty bit | Binary dirty bit |

---

## 3. Design Decisions

### 3.1 Dirty Page List vs Card Table

**Decision**: Dirty page list (not card table)

**Rationale**:
- rudo-gc already has per-object dirty tracking (more precise than cards)
- Card tables add memory overhead and complexity
- Dirty page list is simpler and sufficient for 2-generation design
- Matches Chez Scheme's segment-level tracking pattern

**Alternatives Rejected**:
- Card table: Redundant with existing dirty bitmap, adds complexity
- Lock-free list: Higher complexity, marginal benefit for typical workloads

### 3.2 Synchronization Approach

**Decision**: Mutex-protected Vec with double-check pattern

**Rationale**:
- Chez Scheme uses mutex-protected lists (proven pattern)
- parking_lot::Mutex is fast (uncontended case ~2 cycles)
- Double-check avoids lock acquisition when page already listed
- Lock released before GC scanning (snapshot pattern)

**Memory Ordering**:
- `Acquire` on first flag check (see prior updates)
- `Relaxed` inside mutex (mutex provides synchronization)
- `Release` on flag set (visibility to GC thread)

### 3.3 Duplicate Prevention

**Decision**: Atomic flag in PageHeader (`PAGE_FLAG_DIRTY_LISTED = 0x10`)

**Rationale**:
- Matches Chez Scheme's `min_dirty_byte != 0xff` pattern
- Avoids Vec scan for duplicate detection
- O(1) check before lock acquisition
- Flag bit 0x10 is available in existing `flags` field

### 3.4 Snapshot for Lock-Free Scanning

**Decision**: Copy dirty pages to snapshot Vec at GC start

**Rationale**:
- Lock released immediately after snapshot
- GC can scan without holding lock
- Mutators can continue adding to list
- Pages added during GC caught in next cycle

### 3.5 Large Object Handling

**Decision**: Same treatment as small object pages

**Rationale**:
- Large objects have their own pages
- Page is added to dirty list when large object is mutated
- Trace entire large object (no sub-object precision needed)
- Simplifies implementation

---

## 4. Files to Modify

| File | Changes |
|------|---------|
| `src/heap.rs` | Add `dirty_pages` field to LocalHeap, add `PAGE_FLAG_DIRTY_LISTED`, add `add_to_dirty_pages()` method |
| `src/cell.rs` | Update write barrier to call `add_to_dirty_pages()` |
| `src/gc/gc.rs` | Update `mark_minor_roots*` to use dirty page snapshot |
| `Cargo.toml` | Add parking_lot dependency (if not present) |
| `tests/` | Add new test files for dirty page tracking |

---

## 5. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Mutex contention | Low | Medium | Only lock once per newly-dirty page; parking_lot is fast |
| Race in snapshot | Very Low | Critical | Mutex + ordering ensures atomicity |
| Memory leak in list | Low | Medium | Clear list at end of each GC cycle |
| Page missed in scan | Very Low | Critical | Flag ensures no duplicate adds; snapshot ensures no missed pages |

---

## 6. References

- Chez Scheme GC: `learn-projects/ChezScheme/c/gc.c`, `gc-oce.c`
- rudo-gc current impl: `crates/rudo-gc/src/heap.rs`, `cell.rs`, `gc/gc.rs`
- Design plan: `docs/generational-gc-plan-0.7.2.md`
