# Issue: GcBox Memory Corruption During Incremental Marking

## Summary

During the build loop of a `For` widget rendering 10 items, **GcBox addresses are being prematurely reused** while still referenced. This causes only 6 out of 10 items to display correctly, as indices 2, 3, and 4 have their `Gc<Component>` pointers corrupted (pointing to the same addresses as indices 7, 8, and 9 respectively).

## Affected Versions

- rudo-gc: Latest from `/home/noah/Desktop/rvue/learn-projects/rudo/`
- Feature: Incremental marking enabled (default)

## Reproduction Steps

1. Create a `For` widget with 10+ items
2. Render child components using `Gc::new()` in a loop
3. Store `Gc<Component>` pointers in a `GcCell<Vec<Option<ItemEntry<K, T>>>>`
4. Observe that only ~60% of items render correctly

## Expected vs Actual Behavior

**Expected**: All 10 components have unique `GcBox` addresses.

**Actual**: 
- idx=0: addr=0x7f..., comp_id=7 ✓ UNIQUE
- idx=1: addr=0x7f..., comp_id=11 ✓ UNIQUE
- idx=2: addr=0x7f6df6ce6828, comp_id=15
- idx=3: addr=0x7f6df6ce2828, comp_id=19
- idx=4: addr=0x7f6df6b61828, comp_id=23
- idx=5: addr=0x7f6df6b5c828, comp_id=27
- idx=6: addr=0x7f6df6b58828, comp_id=31
- idx=7: addr=0x7f6df6b54828, comp_id=35 ← **SAME as idx=2 (should be new)**
- idx=8: addr=0x7f6df6b4f828, comp_id=39 ← **SAME as idx=3 (should be new)**
- idx=9: addr=0x7f6df6b4b828, comp_id=43 ← **SAME as idx=4 (should be new)**

## Root Cause Analysis

1. **Missing `GcCapture` for `Gc<T>`**: The `Gc<T>` type did not implement `GcCapture`, so SATB barriers couldn't capture Gc pointers from `Vec<Gc<Component>>`.

2. **Incremental marking timing**: During the build loop, `Gc::new()` calls trigger collections. The incremental marking system fails to properly trace Gc pointers stored in `GcCell<Vec<Gc<Component>>>` because:
   - `Gc<T>` didn't implement `GcCapture`
   - The SATB barrier in `GcCell::borrow_mut()` couldn't capture old values
   - Objects that were still reachable were incorrectly marked as dead

3. **Address reuse**: When a collection runs, unmarked GcBoxes are swept and their memory reused for new allocations.

## Minimal Test Case

```rust
use rudo_gc::{Gc, GcCell, Trace, GcCapture};
use std::cell::RefCell;

#[derive(Clone, Trace)]
struct Item {
    id: u32,
    data: String,
}

impl GcCapture for Item {
    fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<rudo_gc::GcBox<()>>] {
        &[]
    }
    fn capture_gc_ptrs_into(&self, _ptrs: &mut Vec<std::ptr::NonNull<rudo_gc::GcBox<()>>>) {}
}

fn main() {
    // Simulate the For widget build loop
    let items_cell: GcCell<Vec<Option<Gc<Item>>>> = GcCell::new(Vec::new());
    
    for i in 0..10 {
        let item = Gc::new(Item {
            id: i,
            data: format!("item_{}", i),
        });
        
        // Capture the old value for SATB barrier
        let mut items = items_cell.borrow_mut();
        items.push(Some(item.clone()));
        
        // Safepoint to allow GC to run
        rudo_gc::safepoint();
    }
    
    // Verify all addresses are unique
    let items = items_cell.borrow();
    let addrs: Vec<_> = items.iter().filter_map(|o| o.as_ref()).map(|gc| Gc::as_ptr(gc)).collect();
    
    let unique_addrs: std::collections::HashSet<_> = addrs.iter().collect();
    assert_eq!(addrs.len(), unique_addrs.len(), 
        "BUG REPRODUCED: GcBox addresses reused! Got {} unique addresses for 10 items", 
        unique_addrs.len());
}
```

## Workarounds Applied

### 1. Added `GcCapture` implementation for `Gc<T>`

**File**: `/home/noah/Desktop/rvue/learn-projects/rudo/crates/rudo-gc/src/ptr.rs`

```rust
impl<T: Trace> GcCapture for Gc<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        let gc_box_ptr = self.ptr.load(Ordering::Relaxed);
        if !gc_box_ptr.is_null() {
            ptrs.push(gc_box_ptr);
        }
    }
}
```

### 2. Disabled collections during build loop (temporary workaround)

```rust
rudo_gc::set_collect_condition(|_| false);
// Build loop...
rudo_gc::set_collect_condition(rudo_gc::default_collect_condition);
```

## Impact

This bug affects any code that:
1. Creates multiple `Gc<T>` objects in a loop
2. Stores them in a `GcCell<Vec<...>>`
3. Has incremental marking enabled
4. Allows GC to run between allocations

## Suggested Fixes

1. **Add `GcCapture` for `Gc<T>`** - This is the primary fix needed.

2. **Review SATB barrier implementation** - Verify that `GcCell::borrow_mut()` correctly captures old values before mutation when the inner type contains `Gc<T>`.

3. **Add integration test** - Create a test that allocates N Gc objects in a loop and verifies all addresses are unique.

## Environment

- OS: Linux
- Rust: Latest stable
- rudo-gc: Latest from `learn-projects/rudo/`
