# Deep Dive Analysis: `crates/rudo-gc` (Part 4)

## Executive Summary

This phase of the investigation focused on `heap.rs`, specifically `LocalHeap` teardown, large object management, and orphan page handling. I have identified **three critical bugs** causing Use-After-Free (UAF) and memory corruption, particularly affecting multi-threaded scenarios and large object allocations.

## Critical Findings

### 1. Premature Removal of Large Objects from Global Map
**Severity**: Critical
**Location**: `src/heap.rs`, `LocalHeap::drop` (lines 1561-1567).

**Description**:
When a `LocalHeap` is dropped (thread termination), it moves its active pages to the global `orphan_pages` list so they can be reclaimed later. However, for **Large Objects**, it actively **removes** the pages from the `GlobalSegmentManager::large_object_map`.
```rust
                if is_large {
                    let header_addr = header as usize;
                    for p in 0..(size / page_size()) {
                         // ...
                        manager.large_object_map.remove(&page_addr); // <--- ERROR
                    }
                }
```
Conservative stack scanning (`scan.rs` -> `heap.rs::find_gc_box_from_ptr`) relies on this map to identify the start of a large object when given an **interior pointer** (e.g., a pointer to the middle of a large buffer). If the map entry is removed, `find_gc_box_from_ptr` cannot successfully resolve the interior pointer to the object header.

**Scenario**:
1. Thread A creates a Large Object (spanning multiple pages).
2. Thread B holds a reference (pointer) to the middle of this object (2nd page).
3. Thread A terminates (`LocalHeap::drop`). The object is orphaned. Map entries are removed.
4. GC runs. Thread B's stack is scanned. It finds the interior pointer.
5. `find_gc_box_from_ptr` checks the map. Entry missing. It fails to identify the object.
6. The object is not marked suitable.
7. `sweep_orphan_pages` reclaims the object logic.
8. Thread B uses the dangling pointer -> **UAF**.

**Recommendation**:
Do **not** remove entries from `large_object_map` in `LocalHeap::drop`. Only remove them in `sweep_orphan_pages` (and `dealloc`) when the memory is actually unmapped.

### 2. Orphan Sweep Unmaps Weak-Referenced Objects
**Severity**: Critical
**Location**: `src/heap.rs`, `sweep_orphan_pages` (Phase 2).

**Description**:
`sweep_orphan_pages` reclaims pages that have no marks (`!has_survivors`). While this implies no *strong* references exist, it ignores **Weak** references. The function drops the objects and then immediately **unmaps the memory** (`Mmap::from_raw` -> `drop`).
```rust
    // Phase 2: Reclaim memory.
    for (addr, size) in to_reclaim {
        unsafe {
            sys_alloc::Mmap::from_raw(addr as *mut u8, size); // <--- Memory Unmapped
        }
    }
```
If a `Weak` reference to an object in this page exists (e.g., in a global cache), it assumes the allocation (`GcBox`) remains valid (even if the value is dropped). Unmapping the memory invalidates the `GcBox`.

**Impact**:
Calling `upgrade()` or `drop()` on the surviving `Weak` pointer accesses unmapped memory, causing a Segfault or UAF if memory was remapped.

**Recommendation**:
Orphan reclamation must respect weak counts. Since `BiBOP` manages whole pages, we cannot unmap a page if *any* object in it has a non-zero weak count. The logic must check `weak_count > 0` for all allocated objects in the page before deciding to `unmap`. If weak refs exist, the page must be kept (and potentially moved to a "zombie" list or kept in orphans with a flag).

### 3. Write Barrier Failure for Multi-Page Large Objects
**Severity**: High
**Location**: `src/cell.rs`, `GcCell::write_barrier`.

**Description**:
`GcCell` implements the generational write barrier. It uses `ptr_to_page_header` to find the page header and check the generation.
```rust
            let header = ptr_to_page_header(ptr);
            if (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE { ... }
```
`ptr_to_page_header` works by masking the address to the page boundary (`addr & !PAGE_MASK`). For a large object spanning multiple pages, only the **first** page contains a `PageHeader`. If a `GcCell` resides in the **second** page (or later) of a large object, `ptr_to_page_header` returns the address of that page, which contains user data, not a header. The magic check fails (unless random data matches), and the **write barrier is skipped**.

**Impact**:
Old-to-Young pointers in large objects are not tracked.
1. Large Object (Old Gen) holds `GcCell` in 2nd page.
2. `GcCell` updated to point to Young Object.
3. Write Barrier skipped.
4. Minor GC runs. Young Object is not marked (missed root). Swept.
5. Large Object holds dangling pointer -> **UAF**.

**Recommendation**:
`ptr_to_page_header` cannot be used safely on arbitrary pointers if large objects are present.
- **Option A**: Use `find_gc_box_from_ptr` (slow, involves map lookup) in the write barrier.
- **Option B**: Store "Back Pointers" or simplified headers in tail pages of large objects.
- **Option C**: Forbid `GcCell` in large objects (impractical).

---
**R. Kent Dybvig**
*Professor of Computer Science*
