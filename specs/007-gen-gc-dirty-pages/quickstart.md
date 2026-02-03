# Quickstart Guide: Generational GC Dirty Page Tracking

**Feature Branch**: `007-gen-gc-dirty-pages`  
**Date**: 2026-02-03

---

## Overview

This guide provides step-by-step implementation instructions for adding dirty page tracking to rudo-gc. The goal is to optimize minor GC from O(num_pages) to O(dirty_pages).

---

## Prerequisites

1. Check out the feature branch:
   ```bash
   git checkout 007-gen-gc-dirty-pages
   ```

2. Ensure dependencies are available:
   ```bash
   cargo check --workspace
   ```

3. Verify tests pass before changes:
   ```bash
   ./test.sh
   ```

---

## Implementation Steps

### Step 1: Add parking_lot Dependency

**File**: `crates/rudo-gc/Cargo.toml`

```toml
[dependencies]
parking_lot = "0.12"
```

### Step 2: Add PAGE_FLAG_DIRTY_LISTED Constant

**File**: `crates/rudo-gc/src/heap.rs`

Find the existing flag constants (around line 500) and add:

```rust
pub const PAGE_FLAG_LARGE: u8 = 0x01;
pub const PAGE_FLAG_ORPHAN: u8 = 0x02;
#[cfg(feature = "lazy-sweep")]
pub const PAGE_FLAG_NEEDS_SWEEP: u8 = 0x04;
#[cfg(feature = "lazy-sweep")]
pub const PAGE_FLAG_ALL_DEAD: u8 = 0x08;
pub const PAGE_FLAG_DIRTY_LISTED: u8 = 0x10;  // NEW
```

### Step 3: Add PageHeader Methods

**File**: `crates/rudo-gc/src/heap.rs`

Add to the `impl PageHeader` block:

```rust
#[inline]
pub fn is_dirty_listed(&self) -> bool {
    (self.flags.load(Ordering::Acquire) & PAGE_FLAG_DIRTY_LISTED) != 0
}

#[inline]
pub fn set_dirty_listed(&self) {
    self.flags.fetch_or(PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
}

#[inline]
pub fn clear_dirty_listed(&self) {
    self.flags.fetch_and(!PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
}
```

### Step 4: Add Dirty Page Fields to LocalHeap

**File**: `crates/rudo-gc/src/heap.rs`

Add to the `LocalHeap` struct:

```rust
pub struct LocalHeap {
    // ... existing fields ...
    
    /// Pages with dirty objects (old generation only)
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
    
    /// Snapshot for lock-free GC scanning
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    /// Rolling average for capacity planning
    avg_dirty_pages: usize,
    dirty_page_history: [usize; 4],
}
```

Update `LocalHeap::new()` to initialize new fields:

```rust
impl LocalHeap {
    pub fn new() -> Self {
        Self {
            // ... existing initializations ...
            dirty_pages: parking_lot::Mutex::new(Vec::with_capacity(64)),
            dirty_pages_snapshot: Vec::new(),
            avg_dirty_pages: 16,
            dirty_page_history: [16, 16, 16, 16],
        }
    }
}
```

### Step 5: Add LocalHeap Methods

**File**: `crates/rudo-gc/src/heap.rs`

```rust
impl LocalHeap {
    /// Add page to dirty list with double-check pattern
    /// 
    /// # Safety
    /// Caller must ensure header points to valid PageHeader
    #[inline]
    pub unsafe fn add_to_dirty_pages(&self, header: NonNull<PageHeader>) {
        // Fast path: already in list
        if (*header.as_ptr()).is_dirty_listed() {
            return;
        }
        
        // Slow path: acquire lock and double-check
        let mut dirty_pages = self.dirty_pages.lock();
        if !(*header.as_ptr()).is_dirty_listed() {
            dirty_pages.push(header);
            (*header.as_ptr()).set_dirty_listed();
        }
    }
    
    /// Take snapshot for GC scanning
    pub fn take_dirty_pages_snapshot(&mut self) -> usize {
        let mut dirty_pages = self.dirty_pages.lock();
        let capacity = self.avg_dirty_pages.max(16);
        self.dirty_pages_snapshot = Vec::with_capacity(capacity);
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        self.dirty_pages_snapshot.len()
    }
    
    /// Iterate over snapshot
    #[inline]
    pub fn dirty_pages_iter(&self) -> impl Iterator<Item = NonNull<PageHeader>> + '_ {
        self.dirty_pages_snapshot.iter().copied()
    }
    
    /// Clear snapshot and update statistics
    pub fn clear_dirty_pages_snapshot(&mut self) {
        let count = self.dirty_pages_snapshot.len();
        self.dirty_page_history.rotate_right(1);
        self.dirty_page_history[0] = count;
        self.avg_dirty_pages = self.dirty_page_history.iter().sum::<usize>() / 4;
        self.dirty_pages_snapshot.clear();
    }
    
    /// Debug: get dirty page count
    #[cfg(any(test, feature = "gc-debug"))]
    pub fn dirty_pages_count(&self) -> usize {
        self.dirty_pages.lock().len()
    }
}
```

### Step 6: Update Write Barrier

**File**: `crates/rudo-gc/src/cell.rs`

Modify the `write_barrier` function to add page to dirty list:

```rust
fn write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    unsafe {
        crate::heap::with_heap(|heap| {
            let page_addr = (ptr as usize) & crate::heap::page_mask();
            let is_large = heap.large_object_map.contains_key(&page_addr);
            
            if is_large {
                if let Some(&(head_addr, _, _)) = heap.large_object_map.get(&page_addr) {
                    let header = head_addr as *mut crate::heap::PageHeader;
                    if (*header).magic == crate::heap::MAGIC_GC_PAGE
                        && (*header).generation > 0
                    {
                        // ... existing index calculation ...
                        (*header).set_dirty(index);
                        
                        // NEW: Add page to dirty list
                        heap.add_to_dirty_pages(NonNull::new_unchecked(header));
                    }
                }
            } else {
                let header = crate::heap::ptr_to_page_header(ptr);
                if (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE
                    && (*header.as_ptr()).generation > 0
                {
                    // ... existing index calculation ...
                    (*header.as_ptr()).set_dirty(index);
                    
                    // NEW: Add page to dirty list
                    heap.add_to_dirty_pages(header);
                }
            }
        });
    }
}
```

### Step 7: Update mark_minor_roots

**File**: `crates/rudo-gc/src/gc/gc.rs`

Replace the page iteration loop:

```rust
fn mark_minor_roots(heap: &mut LocalHeap) {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);

    // 1. Mark stack roots (unchanged)
    unsafe {
        crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {
            if let Some(gc_box_ptr) =
                crate::heap::find_gc_box_from_ptr(heap, potential_ptr as *const u8)
            {
                mark_object_minor(gc_box_ptr, &mut visitor);
            }
        });

        #[cfg(any(test, feature = "test-util"))]
        TEST_ROOTS.with(|roots| {
            for &ptr in roots.borrow().iter() {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                    mark_object_minor(gc_box, &mut visitor);
                }
            }
        });
    }

    // 2. Take snapshot of dirty pages (NEW)
    let _dirty_count = heap.take_dirty_pages_snapshot();

    // 3. Scan ONLY dirty pages (OPTIMIZED)
    for page_ptr in heap.dirty_pages_iter() {
        unsafe {
            let header = page_ptr.as_ptr();
            
            // Defensive: skip young pages
            if (*header).generation == 0 { continue; }
            
            if (*header).is_large_object() {
                let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
            } else {
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
            
            // Clear dirty state
            (*header).clear_all_dirty();
            (*header).clear_dirty_listed();
        }
    }

    // 4. Clear snapshot (NEW)
    heap.clear_dirty_pages_snapshot();

    visitor.process_worklist();
}
```

### Step 8: Update mark_minor_roots_multi

Apply the same pattern to `mark_minor_roots_multi` (around line 703).

### Step 9: Update mark_minor_roots_parallel

Apply the same pattern to `mark_minor_roots_parallel` (around line 833).

---

## Testing

### Run Existing Tests

```bash
./test.sh
```

### Create New Test File

**File**: `crates/rudo-gc/tests/dirty_page_list.rs`

```rust
use rudo_gc::prelude::*;

#[test]
fn test_dirty_page_basic() {
    // Create objects, trigger mutations, verify dirty tracking
}

#[test]
fn test_old_to_young_survival() {
    // Create old->young reference, run minor GC, verify survival
}

#[test]
fn test_empty_dirty_list() {
    // Run minor GC with no dirty pages
}
```

### Run Miri Tests

```bash
./miri-test.sh
```

---

## Verification Checklist

- [ ] `cargo build --workspace` succeeds
- [ ] `./clippy.sh` passes
- [ ] `./test.sh` passes
- [ ] `./miri-test.sh` passes
- [ ] New tests for dirty page tracking added
- [ ] Benchmarks show improvement (optional)

---

## Common Issues

### Issue: Clippy warning about mutex in struct

**Solution**: Add `#[allow(clippy::mutex_atomic)]` if needed, or use the appropriate clippy allow.

### Issue: Test failures with parallel threads

**Solution**: Ensure tests use `--test-threads=1` as per project conventions.

### Issue: Miri failure on atomic operations

**Solution**: Verify memory ordering is correct (Acquire for loads, Release for stores).

---

## Next Steps

After implementation:

1. Run `/speckit.tasks` to generate task breakdown
2. Create benchmarks comparing before/after performance
3. Run loom tests for concurrency verification
