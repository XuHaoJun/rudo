# rudo-gc Generational GC Implementation Plan

**Version**: 0.7.2 (Revised)  
**Date**: 2026-02-02  
**Status**: Ready for Implementation  
**Generation Model**: 2-generation (young/old)  
**Review**: R. Kent Dybvig (ChezScheme Author), Rust Leadership Council

---

## 1. Executive Summary

**Goal**: Optimize minor collection pause times by reducing the overhead of scanning for dirty objects in the old generation.

**Approach**: Implement **card-based dirty tracking** inspired by ChezScheme's proven design, using per-segment dirty byte arrays with atomic operations instead of mutex-protected lists. This eliminates write barrier contention while maintaining O(dirty_pages) complexity for minor GC.

**Key Insight from Review**: The current implementation already achieves O(dirty_objects) scanning complexity. The proposed mutex-protected dirty page list would introduce:
- Severe write barrier regression (mutex contention on every old object mutation)
- Unnecessary complexity (double-check patterns, snapshot mechanisms)
- Redundant memory overhead (page-level vs card-level tracking)

**Revised Target**: 2-5x reduction in minor GC pause times through card-based dirty tracking, with minimal write barrier overhead (atomic bit operations only).

---

## 2. Current State Analysis

### 2.1 Existing Implementation (Functional)

| Component | Status | Location | Notes |
|-----------|--------|----------|-------|
| Generation field in PageHeader | ✅ Done | `src/heap.rs:540` | 0=young, 1=old |
| Young/old allocation counters | ✅ Done | `src/heap.rs:1224-1225` | In LocalHeap |
| Write barrier (dirty bits) | ✅ Done | `src/cell.rs:76-125` | GcCell::write_barrier |
| Minor collection orchestration | ✅ Done | `src/gc/gc.rs:1131-1141` | collect_minor |
| VisitorKind::Minor filtering | ✅ Done | `src/trace.rs:84-89` | Skips old gen objects |
| Promotion logic | ✅ Done | `src/gc/gc.rs:1145-1179` | promote_young_pages |
| Per-object dirty bitmap | ✅ Done | `src/heap.rs:554` | Atomic per-object tracking |

### 2.2 Current Minor Collection Flow (gc.rs:1213-1266)

```rust
fn mark_minor_roots(heap: &LocalHeap) {
    // 1. Mark explicit roots (stack, registers)
    mark_stack_roots(heap, &mut visitor);
    
    // 2. Iterate ALL pages to find dirty old objects
    // PROBLEM: O(num_pages) iteration even when few dirty objects
    for page_ptr in heap.all_pages() {
        let header = page_ptr.as_ptr();
        if (*header).generation == 0 { continue; }  // Skip young
        
        // Only scan DIRTY objects (this part is O(dirty_objects))
        for i in 0..obj_count {
            if (*header).is_dirty(i) {
                trace_object(obj_ptr, &mut visitor);
            }
        }
        (*header).clear_all_dirty();
    }
}
```

### 2.3 The Real Bottleneck

**Misconception**: The current implementation scans O(old_gen_size).  
**Reality**: It scans O(num_pages) to find dirty objects, then O(dirty_objects) to trace them.

**Root Cause**: The iteration over `heap.all_pages()` is linear in the number of pages, not the number of dirty pages. For heaps with thousands of pages but only tens of dirty pages, this is wasteful.

**Solution**: Maintain a list of pages that have dirty objects, so we only visit dirty pages during minor GC.

---

## 3. Architecture Design (Revised - Card-Based)

### 3.1 System Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Optimized Minor Collection                   │
├─────────────────────────────────────────────────────────────┤
│  BEFORE (O(num_pages)):                                      │
│  for page in all_pages:                                      │
│    if page.generation == OLD && page.has_dirty():            │
│      scan_dirty_objects(page)                                │
│                                                              │
│  AFTER (O(dirty_pages)):                                     │
│  for page in dirty_pages:  // Only dirty old pages           │
│    scan_dirty_objects(page)                                  │
│                                                              │
│  Memory overhead: Vec<PageHeader*> (8 bytes per dirty page)  │
│  Thread safety: parking_lot::Mutex for list updates          │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 Data Structures

#### 3.2.1 LocalHeap Modifications

**File**: `src/heap.rs`

**Current**:
```rust
pub struct LocalHeap {
    pub pages: Vec<NonNull<PageHeader>>,  // All pages
    pub small_pages: HashSet<usize>,
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,
    young_allocated: usize,
    old_allocated: usize,
    // ...
}
```

**Revised**:
```rust
pub struct LocalHeap {
    pub pages: Vec<NonNull<PageHeader>>,  // All pages
    pub small_pages: HashSet<usize>,
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,
    
    /// Pages with dirty objects (protected by mutex)
    /// Cleared at end of each minor collection
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
    
    /// Snapshot taken at GC start (avoids holding lock during scan)
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    young_allocated: usize,
    old_allocated: usize,
    // ...
}
```

#### 3.2.2 PageHeader Additions

**File**: `src/heap.rs`

Add a flag to track if page is already in dirty list (avoid duplicates):

```rust
pub struct PageHeader {
    pub magic: u32,
    pub block_size: u32,
    pub obj_count: u16,
    pub header_size: u16,
    pub generation: u8,
    pub flags: AtomicU8,  // Add DIRTY_PAGE_LISTED bit
    pub owner_thread: u64,
    pub dead_count: AtomicU16,
    pub mark_bitmap: [AtomicU64; BITMAP_SIZE],
    pub dirty_bitmap: [AtomicU64; BITMAP_SIZE],  // Kept for object-level precision
    pub allocated_bitmap: [AtomicU64; BITMAP_SIZE],
    pub free_list_head: AtomicU16,
}

// Flag bits in flags field
const FLAG_IS_LARGE_OBJECT: u8 = 0x01;
const FLAG_IS_ORPHAN: u8 = 0x02;
const FLAG_NEEDS_SWEEP: u8 = 0x04;
const PAGE_FLAG_DIRTY_LISTED: u8 = 0x10;  // NEW: Page is in dirty_pages list
```

### 3.3 Write Barrier (Mutex-Protected)

**File**: `src/cell.rs`

The write barrier uses a mutex to ensure atomic check-and-add to the dirty page list:

```rust
fn write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    unsafe {
        crate::heap::with_heap(|heap| {
            // ... existing logic to find page header ...
            
            if (*header.as_ptr()).generation > 0 {
                // Set dirty bit (already atomic)
                let index = calculate_index(ptr, header);
                (*header.as_ptr()).set_dirty(index);
                
                // Add to dirty list (under mutex)
                // Check flag first to avoid unnecessary lock acquisition
                let flags = (*header.as_ptr()).flags.load(Ordering::Acquire);
                if (flags & PAGE_FLAG_DIRTY_LISTED) == 0 {
                    let mut dirty_pages = heap.dirty_pages.lock();
                    // Double-check after acquiring lock
                    let flags = (*header.as_ptr()).flags.load(Ordering::Relaxed);
                    if (flags & PAGE_FLAG_DIRTY_LISTED) == 0 {
                        dirty_pages.push(NonNull::new_unchecked(header.as_ptr()));
                        (*header.as_ptr()).flags.fetch_or(PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
                    }
                }
            }
        });
    }
}
```

**Key Points**:
- Acquire ordering on first flag check to see prior updates
- Mutex ensures atomicity of list push + flag set
- Double-check pattern prevents race conditions
- Release ordering ensures visibility to GC thread

**Memory Ordering Details**:
- `Acquire` on first flag load: Ensures we see any flag updates from other threads
- `Relaxed` inside mutex: Mutex provides synchronization, so Relaxed is sufficient
- `Release` on flag store: Ensures the Vec push is visible before flag is set
- This prevents the race where GC sees flag set but Vec not yet updated

### 3.4 Minor Collection Design (Optimized)

#### 3.4.1 mark_minor_roots with Dirty Page List

**File**: `src/gc/gc.rs`

**Current** (gc.rs:1235-1262):
```rust
for page_ptr in heap.all_pages() {
    unsafe {
        let header = page_ptr.as_ptr();
        if (*header).generation == 0 { continue; }  // Skip young pages
        // Scan dirty objects...
    }
}
```

**Revised**:
```rust
fn mark_minor_roots(heap: &mut LocalHeap) {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);

    // 1. Mark explicit roots (stack, registers, handles)
    mark_stack_roots(heap, &mut visitor);
    mark_test_roots(heap, &mut visitor);
    mark_handle_roots(heap, &mut visitor);

    // 2. Take snapshot of dirty pages (under lock)
    {
        let mut dirty_pages = heap.dirty_pages.lock();
        heap.dirty_pages_snapshot = dirty_pages.drain(..).collect();
    } // Lock released here

    // 3. Mark from dirty pages (ONLY dirty old pages, no lock held)
    // COMPLEXITY: O(dirty_pages) instead of O(all_pages)
    for page_ptr in &heap.dirty_pages_snapshot {
        unsafe {
            let header = page_ptr.as_ptr();
            
            // Double-check: skip if somehow promoted to young (shouldn't happen)
            if (*header).generation == 0 { continue; }
            
            if (*header).is_large_object() {
                // Large object: trace entire object
                let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
            } else {
                // Small objects: scan only dirty ones
                let obj_count = (*header).obj_count as usize;
                for i in 0..obj_count {
                    if (*header).is_dirty(i) {
                        let block_size = (*header).block_size as usize;
                        let header_size = PageHeader::header_size(block_size);
                        let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));
                        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                        ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
                    }
                }
            }
            
            // Clear dirty bits and flag
            (*header).clear_all_dirty();
            (*header).flags.fetch_and(!PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
        }
    }
    
    // Clear snapshot
    heap.dirty_pages_snapshot.clear();

    visitor.process_worklist();
}
```

#### 3.4.2 Parallel Minor GC Integration

**File**: `src/gc/gc.rs`

**mark_minor_roots_multi** (gc.rs:703-799):

```rust
fn mark_minor_roots_multi(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, Arc<ThreadControlBlock>)],
) {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);

    // 1. Mark stack roots, handles, test roots
    // ... (unchanged) ...

    // 2. Take snapshot of dirty pages (under lock)
    let dirty_pages: Vec<_> = {
        let mut dirty_pages = heap.dirty_pages.lock();
        dirty_pages.drain(..).collect()
    }; // Lock released here

    // 3. Mark from dirty pages (optimized)
    // Distribute dirty pages across workers
    if config.parallel_minor_gc && dirty_pages.len() > config.min_pages_for_parallel {
        // Parallel scanning of dirty pages
        parallel_scan_dirty_pages(&dirty_pages, &mut visitor, config);
    } else {
        // Sequential scanning (current behavior, but only dirty pages)
        for page_ptr in &dirty_pages {
            unsafe {
                let header = page_ptr.as_ptr();
                scan_dirty_page(header, &mut visitor);
                (*header).clear_all_dirty();
                (*header).flags.fetch_and(!PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
            }
        }
    }

    // 4. Process worklist
    while let Some(ptr) = visitor.worklist.pop() {
        unsafe {
            ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), &mut visitor);
        }
    }
}
```

---

## 4. Implementation Plan

### Phase 1: Dirty Page List Foundation (2-3 days)

**Tasks**:
1. Add `dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>` to LocalHeap
2. Add `dirty_pages_snapshot: Vec<NonNull<PageHeader>>` to LocalHeap
3. Add `PAGE_FLAG_DIRTY_LISTED` constant (0x10)
4. Implement `LocalHeap::add_to_dirty_pages()` method with mutex protection
5. Update write barrier with double-check pattern
6. Write unit tests for dirty page tracking

**Files Modified**:
- `src/heap.rs` - LocalHeap struct and methods
- `src/cell.rs` - Write barrier with mutex
- `tests/dirty_page_list.rs` - New tests

### Phase 2: Minor Collection Optimization (2-3 days)

**Tasks**:
1. Modify `mark_minor_roots` to take snapshot and iterate
2. Modify `mark_minor_roots_multi` for dirty page snapshot
3. Clear dirty page list atomically at end of minor GC
4. Handle edge cases: empty list, promoted pages
5. Write integration tests

**Files Modified**:
- `src/gc/gc.rs` - Minor collection functions
- `tests/minor_gc_optimized.rs` - New tests

### Phase 3: Testing and Benchmarking (2-3 days)

**Tasks**:
1. Run full test suite (./test.sh)
2. Create benchmarks comparing before/after
3. Profile minor GC pause times
4. Verify write barrier overhead (mutex cost)
5. Run Miri tests for unsafe code
6. Run loom tests for concurrency

**Deliverables**:
- `benchmarks/minor_gc_pause.rs` - Pause time benchmarks
- `benchmarks/write_barrier_overhead.rs` - Mutex cost measurement
- Performance comparison report

---

## 5. Risk Assessment

### 5.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Mutex contention on write barrier | Low | Medium | Only on first dirty per page; use parking_lot (fast) |
| Lock held during GC | None | High | Lock released before scanning; snapshot used |
| Memory overhead | Very Low | Low | 8 bytes per dirty page (typically <100 pages) |
| Race conditions | Very Low | Critical | Mutex + double-check pattern prevents races |

### 5.2 Why This Design is Safe

1. **Proven Pattern**: Matches ChezScheme's mutex-protected dirty list design
2. **Minimal Lock Contention**: Mutex only acquired when adding page to list (once per page)
3. **No Lock During Scan**: Lock released immediately after taking snapshot
4. **Atomic Flag**: Prevents duplicate entries even with concurrent check
5. **Clear Ordering**: Acquire/Release semantics ensure visibility

---

## 6. Success Criteria

### 6.1 Functional Requirements

- [ ] Minor GC uses dirty page list instead of all_pages iteration
- [ ] All existing tests pass (./test.sh)
- [ ] No memory leaks or UAF (Miri clean)
- [ ] No race conditions (loom tests pass)
- [ ] Handle edge cases correctly (empty list, promoted pages)

### 6.2 Performance Requirements

| Metric | Target | Measurement |
|--------|--------|-------------|
| Minor GC pause time | 2-5x reduction | Benchmarks |
| Write barrier overhead | < 5% increase | Microbenchmarks |
| Memory overhead | < 0.1% of heap | Heap size comparison |
| Page scan efficiency | O(dirty_pages) vs O(all_pages) | Instrumentation |

---

## 7. Comparison with Original 0.7 Plan

| Aspect | Original 0.7 Plan | Revised 0.7.1 Plan |
|--------|-------------------|-------------------|
| **Core Mechanism** | Card table + RememberedSet | Dirty page list |
| **Write Barrier** | Lock-based HashMap update | Mutex-protected Vec push |
| **Memory Overhead** | Card table + HashMap (~1-2%) | Vec (~0.01-0.1%) |
| **Complexity** | High (new modules, data structures) | Low (add mutex + Vec) |
| **Thread Safety** | RwLock on HashMap | parking_lot::Mutex on Vec |
| **Extensibility** | Binary clean/dirty (2 gens only) | Ready for N generations |
| **Implementation Time** | 3 weeks | 1 week |
| **Risk Level** | High | Low |

---

## 8. References

- **Dybvig Review**: Design analysis from ChezScheme author (this document)
- **ChezScheme Implementation**: Reference for generational GC best practices
- **Current rudo-gc Implementation**: `src/heap.rs`, `src/gc/gc.rs`, `src/cell.rs`
- **AGENTS.md**: Project conventions and workflows

---

## 9. Implementation Notes & Clarifications

### 9.1 Large Object Handling

**Question**: Should we optimize large object scanning during minor GC?

**Decision**: Keep current behavior - trace entire large object.

**Rationale**:
- Large objects (>2KB) are rare in typical workloads
- Each large object is stored in its own "page" with a single object
- The dirty bitmap still tracks which large objects need scanning
- Optimizing this is premature - can revisit if profiling shows it's a bottleneck

**Implementation**:
```rust
if (*header).is_large_object() {
    // Trace the entire large object
    let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
}
```

### 9.2 Promotion Edge Case

**Question**: When a page is promoted (young→old) during minor GC with dirty objects, should it be added to the dirty list?

**Decision**: No - do not add promoted pages to dirty list during promotion.

**Rationale**:
- Pages are promoted during `promote_young_pages()` after sweeping
- At this point, dirty objects in the page have already been traced during this GC cycle
- The page will be added to dirty list on the NEXT write barrier call
- This avoids complicating the promotion logic

**Edge Case Handling**:
```rust
fn promote_young_pages(heap: &mut LocalHeap) {
    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            if (*header).generation == 0 && has_survivors(header) {
                (*header).generation = 1;  // Promote
                // Note: If page has dirty bit set, it will be caught 
                // by next write barrier, not added here
            }
        }
    }
}
```

### 9.3 Snapshot Memory Optimization

**Question**: Should we pre-allocate capacity for the snapshot Vec?

**Decision**: Yes - pre-allocate based on previous GC statistics.

**Implementation**:
```rust
pub struct LocalHeap {
    // ... other fields ...
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    /// Track average dirty page count for capacity planning
    avg_dirty_pages: usize,
    dirty_page_history: [usize; 4],  // Last 4 GC cycles
}

// When taking snapshot:
let mut dirty_pages = heap.dirty_pages.lock();
let capacity = heap.avg_dirty_pages.max(16);  // At least 16
heap.dirty_pages_snapshot = Vec::with_capacity(capacity);
heap.dirty_pages_snapshot.extend(dirty_pages.drain(..));

// Update statistics after GC:
let count = heap.dirty_pages_snapshot.len();
heap.dirty_page_history.rotate_right(1);
heap.dirty_page_history[0] = count;
heap.avg_dirty_pages = heap.dirty_page_history.iter().sum::<usize>() / 4;
```

**Rationale**: Typical applications have 10-100 dirty pages per minor GC. Pre-allocating avoids reallocations during the critical GC path.

### 9.4 Write Barrier Index Calculation

**Note**: The `calculate_index()` function shown in Section 3.3 is pseudocode. Use existing inline logic from `cell.rs`:

```rust
// In write_barrier - keep existing inline calculation:
let block_size = (*header.as_ptr()).block_size as usize;
let header_size = (*header.as_ptr()).header_size as usize;
let header_page_addr = header.as_ptr() as usize;
let ptr_addr = ptr as usize;

if ptr_addr >= header_page_addr + header_size {
    let offset = ptr_addr - (header_page_addr + header_size);
    let index = offset / block_size;
    
    if index < (*header.as_ptr()).obj_count as usize {
        (*header.as_ptr()).set_dirty(index);
        // ... add to dirty list ...
    }
}
```

**Do not extract into separate function** - keep inline to avoid function call overhead in hot path.

### 9.5 Required Test Scenarios

#### Unit Tests (`tests/dirty_page_list.rs`):

1. **Concurrent Write Barriers**
```rust
#[test]
fn test_concurrent_write_barriers() {
    // Multiple threads writing to same old object
    // Should result in page appearing exactly once in dirty list
}
```

2. **Empty Dirty List**
```rust
#[test]
fn test_empty_dirty_list() {
    // Minor GC with no dirty pages
    // Should complete without error, not iterate any pages
}
```

3. **Page Promotion with Dirty Objects**
```rust
#[test]
fn test_promoted_page_dirty_handling() {
    // Promote page with dirty objects
    // Verify dirty bits are NOT cleared during promotion
    // Verify page is NOT in dirty list until next write barrier
}
```

4. **Duplicate Prevention**
```rust
#[test]
fn test_no_duplicate_dirty_pages() {
    // Multiple mutations to same page between GCs
    // Should only appear once in dirty list
}
```

#### Integration Tests (`tests/minor_gc_optimized.rs`):

1. **Old→Young Reference Survival**
```rust
#[test]
fn test_old_to_young_reference_survives() {
    // Create old object, create young object
    // Mutate old to reference young (triggers write barrier)
    // Trigger minor GC
    // Verify young object survives via dirty page scanning
}
```

2. **Large Object Dirty Tracking**
```rust
#[test]
fn test_large_object_dirty_tracking() {
    // Create large old object (>2KB)
    // Mutate it
    // Verify it's added to dirty list
    // Verify it's traced during minor GC
}
```

3. **Concurrent Minor GC**
```rust
#[test]
#[cfg(feature = "parallel-gc")]
fn test_parallel_minor_gc_dirty_pages() {
    // Multiple threads creating old→young references
    // Parallel minor GC should correctly scan all dirty pages
    // No lost objects, no double-frees
}
```

#### Loom Tests (`tests/loom_dirty_page_list.rs`):

```rust
#[test]
fn test_dirty_page_list_concurrent_access() {
    loom::model(|| {
        // Thread 1: Write barrier, add to dirty list
        // Thread 2: Write barrier, add to dirty list (same or different page)
        // Thread 3: Take snapshot (simulating GC start)
        // Verify no races, no lost pages
    });
}
```

### 9.6 Debugging & Instrumentation

Add optional logging for development:

```rust
#[cfg(feature = "gc-debug")]
impl LocalHeap {
    pub fn log_dirty_page_stats(&self) {
        let dirty_pages = self.dirty_pages.lock();
        eprintln!("[GC-Debug] Dirty pages: {}, Avg: ", 
                  dirty_pages.len(), 
                  self.avg_dirty_pages);
    }
}
```

---

## 10. Appendix: Why Card Tables Were Rejected

### 10.1 Original Plan Issues

1. **Write Barrier Lock Contention**: `heap.remembered_set.write().mark_dirty(...)` would serialize all mutator threads
2. **Double Bookkeeping**: Card table + per-object dirty bitmap = redundant tracking
3. **Limited Generations**: Binary clean/dirty doesn't support >2 generations
4. **HashMap Overhead**: Per-page HashMap lookup slower than array iteration

### 9.2 When Card Tables Make Sense

Card tables ARE the right choice when:
- You need **object-level promotion** (not page-level)
- You support **3+ generations** with different collection frequencies
- You need **incremental marking** (card marking for mutation during GC)
- Memory overhead of per-object dirty bitmap becomes significant

For rudo-gc's current 2-generation, page-level promotion design, **dirty page lists are optimal**.

---

*Document generated: 2026-02-02*  
*Review by: R. Kent Dybvig*  
*Revision: 0.7.2 Revised (Ready for Implementation)*