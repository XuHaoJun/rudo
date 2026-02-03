# Internal API Contract: Dirty Page Tracking

**Feature Branch**: `007-gen-gc-dirty-pages`  
**Date**: 2026-02-03  
**Type**: Internal Rust API (not public-facing)

---

## Overview

This document defines the internal API contracts for dirty page tracking. These are implementation details, not public API.

---

## 1. PageHeader Extensions

### 1.1 Constants

```rust
// File: src/heap.rs

/// Flag indicating page is in the dirty_pages list
/// Bit 4 of PageHeader.flags
pub const PAGE_FLAG_DIRTY_LISTED: u8 = 0x10;
```

### 1.2 Methods

```rust
impl PageHeader {
    /// Check if page is currently in the dirty pages list
    /// 
    /// # Memory Ordering
    /// Uses Acquire ordering to synchronize with set operations
    #[inline]
    pub fn is_dirty_listed(&self) -> bool {
        (self.flags.load(Ordering::Acquire) & PAGE_FLAG_DIRTY_LISTED) != 0
    }
    
    /// Set the dirty-listed flag (called under mutex)
    /// 
    /// # Memory Ordering
    /// Uses Release ordering to publish Vec push
    #[inline]
    pub fn set_dirty_listed(&self) {
        self.flags.fetch_or(PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
    }
    
    /// Clear the dirty-listed flag (called during GC scan)
    /// 
    /// # Memory Ordering
    /// Uses Release ordering to publish for next cycle
    #[inline]
    pub fn clear_dirty_listed(&self) {
        self.flags.fetch_and(!PAGE_FLAG_DIRTY_LISTED, Ordering::Release);
    }
}
```

---

## 2. LocalHeap Extensions

### 2.1 Fields

```rust
// File: src/heap.rs

pub struct LocalHeap {
    // ... existing fields ...
    
    /// Pages with dirty objects in old generation
    /// Protected by mutex for thread-safe updates
    dirty_pages: parking_lot::Mutex<Vec<NonNull<PageHeader>>>,
    
    /// Snapshot for lock-free scanning during GC
    dirty_pages_snapshot: Vec<NonNull<PageHeader>>,
    
    /// Statistics for capacity planning
    avg_dirty_pages: usize,
    dirty_page_history: [usize; 4],
}
```

### 2.2 Methods

```rust
impl LocalHeap {
    /// Add a page to the dirty pages list if not already present
    /// 
    /// # Contract
    /// - MUST only be called for old-generation pages (generation > 0)
    /// - MUST be called after setting the dirty bit on the object
    /// - Thread-safe: uses mutex + double-check pattern
    /// 
    /// # Performance
    /// - O(1) if page already listed (early exit via flag check)
    /// - O(1) + mutex if adding new page
    /// 
    /// # Safety
    /// - Caller must ensure header is a valid PageHeader pointer
    #[inline]
    pub unsafe fn add_to_dirty_pages(&self, header: NonNull<PageHeader>) {
        // Fast path: check flag without lock
        if (*header.as_ptr()).is_dirty_listed() {
            return;
        }
        
        // Slow path: acquire lock and double-check
        let mut dirty_pages = self.dirty_pages.lock();
        
        // Double-check after acquiring lock (another thread may have added it)
        if !(*header.as_ptr()).is_dirty_listed() {
            dirty_pages.push(header);
            (*header.as_ptr()).set_dirty_listed();
        }
    }
    
    /// Take a snapshot of dirty pages for GC scanning
    /// 
    /// # Contract
    /// - MUST be called at the start of minor GC, before scanning
    /// - Clears the dirty_pages Vec (pages moved to snapshot)
    /// - Lock is released after snapshot, allowing mutators to continue
    /// 
    /// # Returns
    /// Number of pages in the snapshot
    pub fn take_dirty_pages_snapshot(&mut self) -> usize {
        let mut dirty_pages = self.dirty_pages.lock();
        let capacity = self.avg_dirty_pages.max(16);
        self.dirty_pages_snapshot = Vec::with_capacity(capacity);
        self.dirty_pages_snapshot.extend(dirty_pages.drain(..));
        self.dirty_pages_snapshot.len()
    }
    
    /// Get iterator over dirty pages snapshot
    /// 
    /// # Contract
    /// - MUST only be called after take_dirty_pages_snapshot()
    /// - MUST be called before clear_dirty_pages_snapshot()
    #[inline]
    pub fn dirty_pages_iter(&self) -> impl Iterator<Item = NonNull<PageHeader>> + '_ {
        self.dirty_pages_snapshot.iter().copied()
    }
    
    /// Clear the snapshot and update statistics
    /// 
    /// # Contract
    /// - MUST be called at the end of minor GC
    /// - Updates rolling average for capacity planning
    pub fn clear_dirty_pages_snapshot(&mut self) {
        let count = self.dirty_pages_snapshot.len();
        
        // Update statistics
        self.dirty_page_history.rotate_right(1);
        self.dirty_page_history[0] = count;
        self.avg_dirty_pages = self.dirty_page_history.iter().sum::<usize>() / 4;
        
        self.dirty_pages_snapshot.clear();
    }
    
    /// Get count of dirty pages (for debugging/metrics)
    pub fn dirty_pages_count(&self) -> usize {
        self.dirty_pages.lock().len()
    }
}
```

---

## 3. Write Barrier Contract

### 3.1 Updated write_barrier

```rust
// File: src/cell.rs

impl<T: ?Sized> GcCell<T> {
    /// Write barrier for generational GC
    /// 
    /// # Contract
    /// - Sets dirty bit on the containing object
    /// - Adds containing page to dirty list if:
    ///   - Object is in old generation (generation > 0)
    ///   - Page is not already in dirty list
    /// 
    /// # Performance
    /// - Fast path (young gen): ~5 operations, no atomic
    /// - Fast path (old gen, already dirty): ~10 operations, 1 atomic
    /// - Slow path (old gen, first dirty): ~15 operations, 1 atomic, 1 mutex
    fn write_barrier(&self) {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        unsafe {
            crate::heap::with_heap(|heap| {
                // ... existing logic to find page header and check generation ...
                
                if (*header.as_ptr()).generation > 0 {
                    // 1. Set dirty bit (existing behavior)
                    (*header.as_ptr()).set_dirty(index);
                    
                    // 2. NEW: Add page to dirty list
                    heap.add_to_dirty_pages(header);
                }
            });
        }
    }
}
```

---

## 4. Minor GC Contract

### 4.1 Updated mark_minor_roots

```rust
// File: src/gc/gc.rs

/// Mark roots for minor collection (optimized)
/// 
/// # Contract
/// - Takes snapshot of dirty pages at start
/// - Scans ONLY dirty pages (not all pages)
/// - Clears dirty bits and flags after scanning
/// - Clears snapshot at end
/// 
/// # Complexity
/// - O(dirty_pages) + O(dirty_objects) instead of O(all_pages)
fn mark_minor_roots(heap: &mut LocalHeap) {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);

    // 1. Mark stack roots (unchanged)
    mark_stack_roots(heap, &mut visitor);

    // 2. Take snapshot of dirty pages
    let _count = heap.take_dirty_pages_snapshot();

    // 3. Scan ONLY dirty pages
    for page_ptr in heap.dirty_pages_iter() {
        unsafe {
            let header = page_ptr.as_ptr();
            
            // Skip if not old generation (shouldn't happen, defensive)
            if (*header).generation == 0 { continue; }
            
            // Scan dirty objects
            if (*header).is_large_object() {
                // Trace entire large object
                let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                ((*gc_box_ptr).trace_fn)(obj_ptr, &mut visitor);
            } else {
                // Small objects: scan only dirty ones
                for i in 0..(*header).obj_count as usize {
                    if (*header).is_dirty(i) {
                        // ... trace object ...
                    }
                }
            }
            
            // 4. Clear dirty state
            (*header).clear_all_dirty();
            (*header).clear_dirty_listed();
        }
    }

    // 5. Clear snapshot and update stats
    heap.clear_dirty_pages_snapshot();

    visitor.process_worklist();
}
```

---

## 5. Error Conditions

| Condition | Handling |
|-----------|----------|
| Empty dirty page list | Valid state; GC completes quickly |
| Page added during GC | Added to fresh list; processed in next cycle |
| Duplicate page add | Prevented by flag check |
| Invalid page header | Checked via magic number |
| Young page in dirty list | Defensive check skips it |

---

## 6. Thread Safety Guarantees

| Operation | Thread Safety | Mechanism |
|-----------|--------------|-----------|
| `add_to_dirty_pages` | Thread-safe | Mutex + atomic flag |
| `take_dirty_pages_snapshot` | Single-threaded | Called during STW phase |
| `dirty_pages_iter` | Single-threaded | Called during STW phase |
| `clear_dirty_pages_snapshot` | Single-threaded | Called during STW phase |
| Flag check in barrier | Thread-safe | Atomic load with Acquire |
| Flag set in barrier | Thread-safe | Atomic RMW with Release |

---

## 7. Testing Requirements

| Test | Verifies |
|------|----------|
| `test_add_to_dirty_pages` | Page is added correctly |
| `test_duplicate_prevention` | Flag prevents duplicates |
| `test_snapshot_isolation` | Snapshot is independent of list |
| `test_concurrent_write_barriers` | Thread safety under contention |
| `test_empty_dirty_list` | GC handles empty list |
| `test_old_to_young_survival` | Dirty scanning preserves refs |
| `loom_dirty_page_list` | No races under loom |
