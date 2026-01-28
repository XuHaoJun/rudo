# Bug Fix Plan: Critical GC Bugs (Part 4)

## Overview

This document provides a detailed plan for fixing three critical bugs in the `rudo-gc` garbage collector, as identified in `docs/bug-4.md`.

---

## BUG 1: Premature Removal of Large Objects from Global Map

### Root Cause

**Location**: `crates/rudo-gc/src/heap.rs`, `LocalHeap::drop` (lines 1561-1567)

When a `LocalHeap` is dropped (thread termination), it moves active pages to the global `orphan_pages` list for later reclamation. However, for **Large Objects**, the code actively **removes** pages from `GlobalSegmentManager::large_object_map`:

```rust
// crates/rudo-gc/src/heap.rs:1561-1567
if is_large {
    let header_addr = header as usize;
    for p in 0..(size / page_size()) {
        // ...
        manager.large_object_map.remove(&page_addr);  // <--- BUG: Removes too early
    }
}
```

The conservative stack scanner (`scan.rs` -> `find_gc_box_from_ptr`) relies on this map to resolve **interior pointers** (pointers to the middle of a large object). If the map entry is removed prematurely:

1. Thread A creates a Large Object (spanning multiple pages)
2. Thread B holds a reference to the middle (2nd page) of this object
3. Thread A terminates, `LocalHeap::drop` runs, map entries are removed
4. GC runs, Thread B's stack is scanned
5. `find_gc_box_from_ptr` cannot find the object in the map
6. The object is not marked
7. `sweep_orphan_pages` reclaims the object
8. Thread B uses the dangling pointer -> **UAF**

### Test Reproduction Strategy

**Test File**: `crates/rudo-gc/tests/bug1_large_object_interior_uaf.rs`

**Test Function**: `test_thread_termination_with_interior_pointer()`

**Test Approach**: Create a 2-thread scenario where:
1. Thread A allocates a large object and shares an interior pointer with Thread B
2. Thread A terminates while Thread B still holds the interior pointer
3. GC runs (which scans Thread B's stack)
4. Verify the large object is not prematurely collected

```rust
use rudo_gc::{collect_full, Gc, Trace};
use std::thread;

#[repr(C)]
struct LargeStruct {
    data: [u64; 10000],  // ~80KB, spans multiple pages
}

unsafe impl Trace for LargeStruct {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[test]
fn test_thread_termination_with_interior_pointer() {
    let interior_ptr: *const u64;
    let interior_ptr_2: *const u64;

    let handle = thread::spawn(|| {
        let gc = Gc::new(LargeStruct { data: [0x42; 10000] });
        let ptr = std::ptr::from_ref(&gc.data[8500]);
        let ptr2 = std::ptr::from_ref(&gc.data[0]);
        (ptr, ptr2)
    });

    let (ptr, ptr2) = handle.join().unwrap();
    interior_ptr = ptr;
    interior_ptr_2 = ptr2;

    collect_full();

    unsafe {
        assert_eq!(*interior_ptr, 0x42);
        assert_eq!(*interior_ptr_2, 0x42);
    }

    drop(interior_ptr);
    drop(interior_ptr_2);
    collect_full();
}
```

### Fix Approach

**DO NOT** remove entries from `large_object_map` in `LocalHeap::drop`. Only remove entries in `sweep_orphan_pages` when memory is actually unmapped.

**Code Change** (lines 1561-1567):
```rust
// REMOVE this entire block - let sweep_orphan_pages handle cleanup
// if is_large {
//     let header_addr = header as usize;
//     for p in 0..(size / page_size()) {
//         let page_addr = header_addr + (p * page_size());
//         manager.large_object_map.remove(&page_addr);
//     }
// }
```

**Files to Modify**:
- `crates/rudo-gc/src/heap.rs` (lines 1561-1567)

### Verification

After fix, run:
```bash
cargo test --test bug1_large_object_interior_uaf -- --test-threads=1
```

**Expected Assertions**:
- Object survives after Thread A terminates
- Interior pointer can be safely dereferenced
- No UAF or segfault

---

## BUG 2: Orphan Sweep Unmaps Weak-Referenced Objects

### Root Cause

**Location**: `crates/rudo-gc/src/heap.rs`, `sweep_orphan_pages` (Phase 2, lines 1645-1649)

The `sweep_orphan_pages` function reclaims pages with no marks (`!has_survivors`). This correctly handles **strong** references but ignores **weak** references. After dropping objects, it immediately unmaps memory:

```rust
// crates/rudo-gc/src/heap.rs:1645-1649
for (addr, size) in to_reclaim {
    unsafe {
        sys_alloc::Mmap::from_raw(addr as *mut u8, size);
    }
}
```

If a `Weak` reference exists to an object in this page:
1. The strong refs are gone, so `has_survivors` is `false`
2. Objects are dropped, but `weak_count > 0`
3. Memory is unmapped
4. The `Weak` reference's `GcBox` pointer now points to unmapped memory
5. Calling `upgrade()` or `drop()` on the `Weak` pointer -> **UAF/Segfault**

### Test Reproduction Strategy

**Test File**: `crates/rudo-gc/tests/bug2_orphan_sweep_weak_ref.rs`

**Test Function**: `test_weak_ref_survives_orphan_sweep()`

```rust
use rudo_gc::{collect_full, Gc, Trace, Weak};
use std::thread;

#[repr(C)]
struct LargeStruct {
    data: [u64; 10000],
}

unsafe impl Trace for LargeStruct {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[test]
fn test_weak_ref_survives_orphan_sweep() {
    let weak_ref: Weak<LargeStruct>;

    let handle = thread::spawn(|| {
        let gc = Gc::new(LargeStruct { data: [0xCC; 10000] });
        let weak = Gc::downgrade(&gc);
        weak
    });

    weak_ref = handle.join().unwrap();

    assert!(weak_ref.is_alive());

    let upgraded = weak_ref.upgrade();
    assert!(upgraded.is_some());
    assert_eq!(upgraded.unwrap().data[0], 0xCC);

    drop(upgraded);
    drop(weak_ref);
    collect_full();
}
```

### Fix Approach

Before unmapping a page in `sweep_orphan_pages`, check if any object has `weak_count > 0`.

```rust
// In sweep_orphan_pages, before Phase 2:
let has_weak_refs = unsafe {
    // Check weak_count for all objects in the page
    // For large objects: check the single object's weak_count
    // For small objects: check all objects' weak_counts
};

if !has_weak_refs {
    to_reclaim.push((addr, size));
}
```

**Files to Modify**:
- `crates/rudo-gc/src/heap.rs` (`sweep_orphan_pages` function, lines 1582-1650)

### Verification

After fix, run:
```bash
cargo test --test bug2_orphan_sweep_weak_ref -- --test-threads=1
```

**Expected Assertions**:
- Weak ref remains alive after orphan sweep
- `upgrade()` succeeds and returns valid data
- No segfault or UAF

---

## BUG 3: Write Barrier Failure for Multi-Page Large Objects

### Root Cause

**Location**: `crates/rudo-gc/src/cell.rs`, `GcCell::write_barrier` (lines 77-114)

`GcCell` uses `ptr_to_page_header` which masks to page boundary. For multi-page large objects, **only the first page** contains a `PageHeader`. If a `GcCell` resides in the **second page or later**:
1. `ptr_to_page_header` returns the address of that page
2. That page contains user data, not a header
3. The magic check fails
4. The write barrier is **skipped**
5. Old-to-Young pointers are not tracked
6. Minor GC misses the young object -> **UAF**

### Test Reproduction Strategy

**Test File**: `crates/rudo-gc/tests/bug3_write_barrier_multi_page.rs`

**Test Function**: `test_gccell_write_barrier_in_second_page()`

```rust
use rudo_gc::{collect_full, Gc, Trace};
use rudo_gc::cell::GcCell;

#[repr(C)]
struct LargeStructWithGccell {
    _padding: [u64; 7000],
    cell: GcCell<u32>,
}

unsafe impl Trace for LargeStructWithGccell {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {
        self.cell.trace(_visitor);
    }
}

#[test]
fn test_gccell_write_barrier_in_second_page() {
    let gc = Gc::new(LargeStructWithGccell {
        _padding: [0; 7000],
        cell: GcCell::new(0),
    });

    let cell_addr = std::ptr::from_ref(&gc.cell) as usize;
    let page_size = rudo_gc::heap::page_size();
    let head_page = (Gc::as_ptr(&gc) as usize) & !page_size;
    let cell_page = cell_addr & !page_size;

    assert_ne!(head_page, cell_page);

    *gc.cell.borrow_mut() = 123;

    collect_full();

    assert_eq!(*gc.cell.borrow(), 123);
}
```

### Fix Approach

Use `find_gc_box_from_ptr` in `GcCell::write_barrier`:

```rust
fn write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    unsafe {
        crate::heap::with_heap(|heap| {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                let header = gc_box.as_ptr() as *mut PageHeader;
                if (*header).magic == crate::heap::MAGIC_GC_PAGE {
                    if (*header).generation > 0 {
                        // Set dirty bit...
                    }
                }
            }
        });
    }
}
```

**Files to Modify**:
- `crates/rudo-gc/src/cell.rs` (`write_barrier` function, lines 77-114)

### Verification

After fix, run:
```bash
cargo test --test bug3_write_barrier_multi_page -- --test-threads=1
```

**Expected Assertions**:
- GcCell in second page correctly triggers write barrier
- No corruption or UAF

---

## Order of Operations

### Phase 1: Test First
1. Create `tests/bug1_large_object_interior_uaf.rs` - run to confirm bug exists
2. Create `tests/bug2_orphan_sweep_weak_ref.rs` - run to confirm bug exists
3. Create `tests/bug3_write_barrier_multi_page.rs` - run to confirm bug exists

### Phase 2: Fix Implementation
1. Fix Bug 1: Remove large_object_map removal in LocalHeap::drop
2. Fix Bug 2: Add weak_count check in sweep_orphan_pages
3. Fix Bug 3: Update GcCell::write_barrier to use find_gc_box_from_ptr

### Phase 3: Verification
1. Run all bug reproduction tests - should pass
2. Run `./test.sh` - all tests should pass
3. Run `./clippy.sh` - no warnings

---

## Existing Test Patterns to Follow

From existing tests in `crates/rudo-gc/tests/`:

1. **Thread termination**: `thread_termination.rs` - uses `thread::spawn` pattern
2. **Large objects**: `large_object.rs` - uses `#[repr(C)]` structs with large arrays
3. **Weak references**: `weak.rs` - uses `Gc::downgrade()` and `Weak::upgrade()`
4. **Test utilities**: `register_test_root`, `clear_test_roots` from `rudo_gc::test_util`

---

## Summary

| Bug | File | Lines | Change |
|-----|------|-------|--------|
| 1 | `crates/rudo-gc/src/heap.rs` | 1561-1567 | Remove large_object_map removal |
| 2 | `crates/rudo-gc/src/heap.rs` | 1582-1650 | Add weak_count check |
| 3 | `crates/rudo-gc/src/cell.rs` | 77-114 | Use find_gc_box_from_ptr |
