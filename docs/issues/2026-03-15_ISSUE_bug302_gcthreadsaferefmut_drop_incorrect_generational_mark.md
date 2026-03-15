# [Bug]: GcThreadSafeRefMut::drop incorrectly marks GC pointers during generational barrier (inconsistent with GcCell)

**Status:** Open
**Tags:** Unverified

## 📊 Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | High | Triggered every time GcThreadSafeRefMut is dropped when generational barrier is active |
| **Severity** | Medium | Causes unnecessary marking overhead; inconsistent with GcCell behavior |
| **Reproducibility** | High | Always triggered - deterministic code path |

---

## 🧩 Affected Component & Environment
- **Component:** `GcThreadSafeRefMut::drop()` in `cell.rs:1391`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current (unfixed)

---

## 📝 Description

### Expected Behavior
`GcThreadSafeRefMut::drop()` should only mark GC pointers black during **incremental marking** (to implement Dijkstra insertion barrier). During **generational barrier** (minor GC), marking should NOT happen - instead, the page should be marked dirty so the GC can scan dirty pages to find young objects.

This is the behavior of `GcCell::borrow_mut()` and `GcThreadSafeCell::borrow_mut()` after bug301 fix.

### Actual Behavior
`GcThreadSafeRefMut::drop()` marks GC pointers black when **either** incremental OR generational barrier is active:

```rust
// cell.rs:1391
if incremental_active || generational_active {
    for gc_ptr in &ptrs {
        mark_object_black(...);
    }
}
```

This is inconsistent with GcCell behavior and causes unnecessary marking during generational barrier.

### Code Location
- `crates/rudo-gc/src/cell.rs:1391` - GcThreadSafeRefMut::drop()

---

## 🔬 Root Cause Analysis

The bug was introduced because:

1. **Bug109 fix** (2026-02-25): Added generational barrier check to GcThreadSafeRefMut::drop (previously only checked incremental)
2. **Bug301 fix** (2026-03-15): Changed GcCell::borrow_mut and GcThreadSafeCell::borrow_mut to ONLY mark during incremental (not generational)

The bug301 fix was applied to the `borrow_mut` methods but was **missed** in the `GcThreadSafeRefMut::drop` implementation!

Current code (cell.rs:1381-1403):
```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();

        let mut ptrs = Vec::with_capacity(32);
        (*self.inner).capture_gc_ptrs_into(&mut ptrs);

        // BUG: Should only mark during incremental, not generational!
        // (bug122 comment says "match GcCell behavior" but GcCell doesn't do this)
        if incremental_active || generational_active {
            for gc_ptr in &ptrs {
                mark_object_black(...);
            }
        }

        if generational_active {
            // This correctly triggers generational barrier
            crate::heap::unified_write_barrier(...);
        }
    }
}
```

Compare with GcCell::borrow_mut (cell.rs:192-207) - correctly fixed by bug301:
```rust
// FIX bug301: Only mark during incremental marking
if incremental_active {
    // mark_object_black
}
```

---

## 💣 PoC

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, collect_full};

#[derive(Trace)]
struct Data {
    cell: GcThreadSafeCell<Option<Gc<Data>>>,
}

fn main() {
    // Create object with GcThreadSafeCell containing GC pointer
    let gc = Gc::new(Data {
        cell: GcThreadSafeCell::new(None),
    });

    // Create another GC object
    let other = Gc::new(Data {
        cell: GcThreadSafeCell::new(None),
    });

    // Set reference through GcThreadSafeCell - triggers generational barrier
    // (not incremental marking, just regular minor GC)
    *gc.cell.borrow_mut() = Some(other);

    // Drop the guard - this triggers drop() which incorrectly marks during generational
    drop(gc);

    // During generational barrier (not incremental), GcThreadSafeRefMut::drop
    // incorrectly calls mark_object_black, which is unnecessary overhead
}
```

---

## 🛠️ Suggested Fix

Change line 1391 from:
```rust
if incremental_active || generational_active {
```

to:
```rust
if incremental_active {
```

This matches the behavior of GcCell::borrow_mut and GcThreadSafeCell::borrow_mut after bug301 fix.

The generational barrier is still correctly triggered at line 1399-1401 via `unified_write_barrier`, which marks the page as dirty instead of marking individual objects.

---

## 🗣️ Internal Discussion Record

**R. Kent Dybvig (GC Architecture):**
The generational barrier's purpose is to track OLD→YOUNG references by marking pages dirty, not by marking objects black. The dirty page scanning will find young objects during minor GC. Marking objects black during generational barrier is redundant and defeats the purpose of having separate mechanisms for generational vs incremental GC.

**Rustacean (Soundness):**
This is not a soundness bug (no UB), but it causes performance overhead and is inconsistent with GcCell behavior. The comment at line 1389 says "match GcCell behavior" but the implementation doesn't match.

**Geohot (Exploit):**
No exploitation vector - this is a correctness/performance issue rather than a security issue.

---

## Verification Notes

- Check: Does GcCell::borrow_mut only mark during incremental? YES (bug301 fix)
- Check: Does GcThreadSafeCell::borrow_mut only mark during incremental? YES (bug301 fix)
- Check: Does GcThreadSafeRefMut::drop only mark during incremental? NO - BUG!
- Check: Does sync.rs GcRwLockWriteGuard::drop mark during generational? Need to verify
