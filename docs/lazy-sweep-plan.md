# Lazy Sweep Implementation Plan for rudo-gc

**Date:** 2026-01-31
**Author:** R. Kent Dybvig (assistant role-play)
**Status:** Ready for Implementation

## Executive Summary

Replace synchronous full-heap sweep during collection with incremental lazy sweep performed during allocation. This reduces GC pause times by spreading sweep work over normal allocation operations.

**Key Benefits:**
- Eliminate STW sweep phase (pause time: O(pages+objects) → O(1) amortized)
- Better cache locality (sweep pages you're actively using)
- Simpler multi-threaded coordination (no concurrent sweep)

**Key Tradeoffs:**
- Memory reclaimed later (on next allocation, not immediately)
- Slightly higher fragmentation (acceptable for most workloads)

---

## 1. Design Decisions

| Aspect | Decision |
|--------|----------|
| Default behavior | Lazy sweep enabled |
| Batch size | 16 objects per page |
| Sweep triggers | Both `check_safepoint()` and `sweep_pending()` API |
| Feature flag | `lazy-sweep` (default: true) |
| Eager sweep | Kept for testing/benchmarking and large objects/orphans |

### Why Lazy Sweep Over Parallel Sweep?

For the stated goals ("reduce GC pause times" + "short-lived objects"):
- **Lazy sweep** eliminates STW pause entirely
- **Parallel sweep** reduces but doesn't eliminate pauses
- Lazy sweep adapts to allocation pattern naturally
- Simpler correctness (less synchronization)

---

## 2. Data Structure Changes

### 2.1 Add Flags

**File:** `crates/rudo-gc/src/heap.rs` (line ~426)

```rust
pub const PAGE_FLAG_LARGE: u8 = 0x01;
pub const PAGE_FLAG_ORPHAN: u8 = 0x02;
pub const PAGE_FLAG_NEEDS_SWEEP: u8 = 0x04;  // NEW: Page has dead objects
pub const PAGE_FLAG_ALL_DEAD: u8 = 0x08;     // NEW: Optimization - all objects in page are dead
```

### 2.2 Add Helper Methods to PageHeader

**File:** `crates/rudo-gc/src/heap.rs` (in `impl PageHeader`)

```rust
/// Check if page needs sweeping.
#[inline]
#[must_use]
pub const fn needs_sweep(&self) -> bool {
    (self.flags & PAGE_FLAG_NEEDS_SWEEP) != 0
}

/// Set page as needing sweep.
#[inline]
pub fn set_needs_sweep(&mut self) {
    self.flags |= PAGE_FLAG_NEEDS_SWEEP;
}

/// Clear sweep-needed flag.
#[inline]
pub fn clear_needs_sweep(&mut self) {
    self.flags &= !PAGE_FLAG_NEEDS_SWEEP;
}

/// Check if all objects in page are known dead (optimization).
#[inline]
#[must_use]
pub fn all_dead(&self) -> bool {
    (self.flags & PAGE_FLAG_ALL_DEAD) != 0
}

/// Set all-dead flag.
#[inline]
pub fn set_all_dead(&mut self) {
    self.flags |= PAGE_FLAG_ALL_DEAD;
}

/// Clear all-dead flag.
#[inline]
pub fn clear_all_dead(&mut self) {
    self.flags &= !PAGE_FLAG_ALL_DEAD;
}
```

### 2.3 Add Per-Page Dead Object Counter

**File:** `crates/rudo-gc/src/heap.rs` (in `PageHeader` struct)

```rust
pub struct PageHeader {
    // ... existing fields ...
    // Optimization: Replaces _padding: [u8; 2] to maintain struct size/alignment
    pub dead_count: Cell<u16>,  // NEW: Count of dead objects
    pub free_list_head: Option<u16>,
}
```

**Rationale:** Track how many dead objects are in a page to enable "all-dead" fast path. By using existing padding, we assume no extra memory overhead per page.

---

## 3. Algorithm Changes

### 3.1 Modify Mark Phase

**File:** `crates/rudo-gc/src/gc/gc.rs`

**Current behavior (eager sweep):**
```
1. Clear all marks
2. Mark reachable objects
3. Sweep ALL pages (STW) - expensive
```

**New behavior (lazy sweep):**
```
1. Clear all marks
2. Mark reachable objects
3. Set needs_sweep flag on all pages (O(pages))
4. Return immediately - no STW sweep
```

**Implementation:**

```rust
// In perform_multi_threaded_collect() after marking:
for tcb in &tcbs {
    for page_ptr in tcb.heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            // Only mark pages that have allocated objects
            if (*header).free_list_head.is_none() || (*header).allocated_bitmap != [0; BITMAP_SIZE] {
                (*header).set_needs_sweep();
            }
        }
    }
}
```

### 3.2 Create Lazy Sweep Functions

**File:** `crates/rudo-gc/src/gc/gc.rs`

```rust
// ============================================================================
// Lazy Sweep Implementation
// ============================================================================

pub const ~~SWEEP_BATCH_SIZE~~: usize = ~~16~~; **(REMOVED: Batch limit removed due to bugs in breakpoint recovery)**

/// Lazily sweep dead objects from a page during allocation.
/// Returns true if any objects were reclaimed.
#[inline(never)]
pub unsafe fn lazy_sweep_page(
    heap: &mut LocalHeap,
    page_ptr: NonNull<PageHeader>,
) -> bool {
    let header = page_ptr.as_ptr();

    // Skip large objects - sweep eagerly (all-or-nothing)
    if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {
        return false;
    }

    // If marked as "all dead", use fast path
    if (*header).all_dead() {
        return lazy_sweep_page_all_dead(heap, page_ptr);
    }

    let block_size = (*header).block_size as usize;
    let obj_count = (*header).obj_count as usize;
    let header_size = PageHeader::header_size(block_size);

    let mut reclaimed = 0;
    let mut found_dead = false;

    for i in 0..obj_count {
        // ~~if reclaimed >= SWEEP_BATCH_SIZE { break; }~~ (REMOVED: Batch limit removed)

        let is_alloc = (*header).is_allocated(i);
        let is_marked = (*header).is_marked(i);

        if is_alloc && !is_marked {
            // Dead object - process it
            let obj_ptr = page_ptr.as_ptr().cast::<u8>()
                .add(header_size + i * block_size);
            let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

            let weak_count = (*gc_box_ptr).weak_count();

            if weak_count > 0 {
                // Has weak refs - drop value but keep allocation
                if !(*gc_box_ptr).is_value_dead() {
                    ((*gc_box_ptr).drop_fn)(obj_ptr);
                    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                    (*gc_box_ptr).set_dead();
                }
            } else {
                // No weak refs - fully reclaim
                ((*gc_box_ptr).drop_fn)(obj_ptr);
                (*gc_box_ptr).set_dead();

                // Add to free list
                let current_free = (*header).free_list_head;
                obj_ptr.cast::<Option<u16>>().write_unaligned(current_free);
                (*header).free_list_head = Some(u16::try_from(i).unwrap());
                (*header).clear_allocated(i);
                reclaimed += 1;
                found_dead = true;
            }
        }
    }

    // Clear mark bits for surviving objects
    for i in 0..obj_count {
        if (*header).is_marked(i) {
            (*header).clear_mark(i);
        }
    }

    // Check if page is now empty
    if found_dead {
        let allocated_count = (*header).allocated_bitmap
            .iter()
            .fold(0, |acc, word| acc + word.count_ones() as usize);

        if allocated_count == 0 {
            (*header).clear_all_marks();
            (*header).set_all_dead();
            (*header).clear_needs_sweep();
        }
    }

    found_dead
}

/// Fast path: Page is entirely dead, just rebuild free list.
#[inline(never)]
pub unsafe fn lazy_sweep_page_all_dead(
    heap: &mut LocalHeap,
    page_ptr: NonNull<PageHeader>,
) -> bool {
    let header = page_ptr.as_ptr();
    let block_size = (*header).block_size as usize;
    let obj_count = (*header).obj_count as usize;
    let header_size = PageHeader::header_size(block_size);

    let mut free_head: Option<u16> = None;
    for i in (0..obj_count).rev() {
        let obj_ptr = page_ptr.as_ptr().cast::<u8>()
            .add(header_size + i * block_size);

        // Drop the value
        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
        ((*gc_box_ptr).drop_fn)(obj_ptr);

        // Add to free list
        obj_ptr.cast::<Option<u16>>().write_unaligned(free_head);
        free_head = Some(u16::try_from(i).unwrap());
        (*header).clear_allocated(i);
    }

    (*header).free_list_head = free_head;
    (*header).clear_all_marks();
    (*header).clear_all_dead();
    (*header).clear_needs_sweep();

    true
}

/// Sweep up to `count` pages that need sweeping.
pub fn sweep_pending(heap: &mut LocalHeap, count: usize) -> usize {
    let mut swept = 0;
    for page_ptr in heap.all_pages() {
        if swept >= count {
            break;
        }
        unsafe {
            if (*page_ptr.as_ptr()).needs_sweep() {
                if lazy_sweep_page(heap, page_ptr) {
                    swept += 1;
                }
            }
        }
    }
    swept
}

/// Get the number of pages pending sweep.
pub fn pending_sweep_count(heap: &LocalHeap) -> usize {
    heap.all_pages()
        .filter(|p| unsafe { p.as_ptr().needs_sweep() })
        .count()
}
```

### 3.3 Integrate with Allocation Path (Critical)

**File:** `crates/rudo-gc/src/heap.rs`

To prevent unbounded heap growth, the allocator **must** attempt to reclaim memory via lazy sweeping before requesting new pages from the OS.

**New Helper Method:**

```rust
// In impl LocalHeap
fn alloc_from_pending_sweep(&mut self, class_index: usize) -> Option<NonNull<u8>> {
    let block_size = SIZE_CLASSES[class_index];
    
    // Scan pages to find one that needs sweeping
    // Optimization TODO: Use a cursor or separate list to avoid O(N) scan every time
    // For MVP: Simple iteration is acceptable as fallback
    for page_ptr in &self.pages {
        unsafe {
            let header = page_ptr.as_ptr();
            // Match block size and check sweep flag
            if ((*header).flags & crate::heap::PAGE_FLAG_LARGE) == 0
                && (*header).block_size as usize == block_size
                && (*header).needs_sweep() 
            {
                // Found a page that needs sweeping. Sweep a batch!
                // If we reclaim objects, they go into the free list.
                if crate::gc::lazy_sweep_page(self, *page_ptr) {
                    // Try to allocate from the now-populated free list
                    if let Some(ptr) = self.alloc_from_free_list(class_index) {
                        return Some(ptr);
                    }
                }
            }
        }
    }
    None
}
```

**Modified `alloc` Method:**

```rust
pub fn alloc<T>(&mut self) -> NonNull<u8> {
    // ... existing TLAB and free list checks ...

    // 1. Try TLAB
    // ...

    // 2. Try Free List
    if let Some(ptr) = self.alloc_from_free_list(class_index) {
        // ...
        return ptr;
    }

    // 3. NEW: Try Lazy Sweep (Reclaim before asking OS)
    if let Some(ptr) = self.alloc_from_pending_sweep(class_index) {
         self.update_range(ptr.as_ptr() as usize & page_mask(), page_size());
         return ptr;
    }

    // 4. Alloc Slow (New Page)
    let ptr = self.alloc_slow(size, class_index);
    // ...
}
```

### 3.4 Integrate Safepoint Trigger

**File:** `crates/rudo-gc/src/heap.rs` in `check_safepoint()`

```rust
#[cfg(feature = "lazy-sweep")]
pub fn check_safepoint() {
    // ... existing code ...

    // Occasionally do lazy sweep work during safepoint checks
    // ~0.5% chance per allocation
    if crate::gc::should_do_lazy_sweep() {
        let mut count = 0;
        HEAP.with(|h| {
            unsafe {
                let heap = &mut *h.tcb.heap.get();
                count = crate::gc::sweep_pending(heap, 4);
            }
        });
    }
}

#[cfg(not(feature = "lazy-sweep"))]
pub fn check_safepoint() {
    // ... existing code unchanged ...
}
```

### 3.4 Large Objects and Orphans (Keep Eager)

Large objects are all-or-nothing, orphan pages must be reclaimed promptly. These continue using eager sweep.

---

## 4. Public API

**File:** `crates/rudo-gc/src/lib.rs`

```rust
#[cfg(feature = "lazy-sweep")]
pub fn sweep_pending(num_pages: usize) -> usize {
    HEAP.with(|h| {
        unsafe {
            let heap = &mut *h.tcb.heap.get();
            crate::gc::sweep_pending(heap, num_pages)
        }
    })
}

#[cfg(feature = "lazy-sweep")]
pub fn pending_sweep_pages() -> usize {
    HEAP.with(|h| {
        unsafe {
            let heap = &*h.tcb.heap.get();
            crate::gc::pending_sweep_count(heap)
        }
    })
}
```

---

## 5. Cargo.toml Configuration

```toml
[features]
default = ["lazy-sweep", "derive"]
lazy-sweep = []  # When disabled, use eager sweep (for testing)
derive = []
```

---

## 6. Testing

### New Test File: `tests/lazy_sweep.rs`

```rust
use rudo_gc::{collect, sweep_pending, Gc, Trace};

#[test]
fn test_lazy_sweep_frees_dead_objects() { ... }

#[test]
fn test_lazy_sweep_preserves_live_objects() { ... }

#[test]
fn test_lazy_sweep_all_dead_optimization() { ... }

#[test]
fn test_lazy_sweep_weak_refs() { ... }

#[test]
fn test_lazy_sweep_minor_gc() { ... }

#[test]
fn test_lazy_sweep_major_gc() { ... }

#[test]
fn test_lazy_sweep_large_object_still_eager() { ... }

#[test]
fn test_lazy_sweep_orphan_still_eager() { ... }
```

### New Benchmark: `tests/benchmarks/sweep_comparison.rs`

```rust
use rudo_gc::{collect, Gc, Trace};

#[bench]
fn bench_sweep_eager_pause_time(b: &mut Bencher) { ... }

#[bench]
fn bench_sweep_lazy_pause_time(b: &mut Bencher) { ... }

#[bench]
fn bench_sweep_eager_throughput(b: &mut Bencher) { ... }

#[bench]
fn bench_sweep_lazy_throughput(b: &mut Bencher) { ... }
```

---

## 7. Implementation Schedule

| Day | Task | Deliverable |
|-----|------|-------------|
| 1 | Infrastructure | Flags, methods, `dead_count` field added |
| 2 | Core implementation | `lazy_sweep_page()` functions working |
| 3 | Integration | Mark phase modified, triggers integrated, API added |
| 4 | Testing & tuning | Tests pass, benchmarks running |

---

## 8. Key Invariants

1. **Mark bits valid until sweep**: Objects remain marked until their page is lazily swept
2. **Allocated bit valid until reclaim**: Objects remain allocated until reclaimed
3. **Weak refs survive**: Objects with weak refs keep allocation until weak ref is dropped
4. **Orphan pages reclaimed promptly**: Orphan sweep remains eager
5. **Large objects all-or-nothing**: Large object sweep remains eager

---

## 9. Complexity Analysis

| Aspect | Eager (current) | Lazy (new) |
|--------|-----------------|------------|
| **Pause time** | O(pages + objects) | O(1) amortized |
| **Implementation complexity** | Medium | Medium |
| **Testing complexity** | Low | Medium |
| **Risk of bugs** | Low | Medium |
| **Memory overhead** | 0 | 1 flag byte/page + 2 bytes/count |

---

## 10. Rollout Strategy

1. Feature flag `lazy-sweep` defaults to enabled
2. Benchmarks compare eager vs lazy sweep
3. Users can disable with `--no-default-features --features derive`
4. Documentation explains lazy sweep behavior

---

## 11. Files to Modify

| File | Change |
|------|--------|
| `crates/rudo-gc/src/heap.rs` | Add flags, `dead_count`, modify `alloc` to use `alloc_from_pending_sweep` |
| `crates/rudo-gc/src/gc/gc.rs` | Modify mark phase, add lazy sweep functions |
| `crates/rudo-gc/src/lib.rs` | Add public API (`sweep_pending`, `pending_sweep_pages`) |
| `crates/rudo-gc/Cargo.toml` | Add `lazy-sweep` feature (default: true) |
| `crates/rudo-gc/tests/lazy_sweep.rs` | New test file |
| `crates/rudo-gc/tests/benchmarks/sweep_comparison.rs` | New benchmark file |

---

## 12. Questions & Answers

**Q: Should lazy sweep be the default?**
A: Yes, with eager as fallback via feature flag.

**Q: Batch size?**
A: 16 objects per page.

**Q: Sweep trigger?**
A: Both `check_safepoint()` and `sweep_pending()` API.

**Q: Keep eager sweep?**
A: Yes, for testing/benchmarking and large objects/orphans.
