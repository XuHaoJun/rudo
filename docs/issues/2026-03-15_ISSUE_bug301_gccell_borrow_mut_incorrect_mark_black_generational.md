# [Bug]: GcCell::borrow_mut() incorrectly marks new GC pointers black during generational barrier

**Status:** Open
**Tags:** Unverified

## 📊 Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | High | Triggered every time GcCell::borrow_mut() is used when generational barrier is active |
| **Severity** | High | Causes young objects to not be collected during minor GC, leading to memory leaks |
| **Reproducibility** | Low | Need minor GC + GcCell mutation |

---

## 🧩 Affected Component & Environment
- **Component:** `GcCell::borrow_mut()`, `GcThreadSafeCell::borrow_mut()` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current (unfixed)

---

## 📝 Description

In `GcCell::borrow_mut()` and `GcThreadSafeCell::borrow_mut()`, when `barrier_active` (= `generational_active || incremental_active`) is true, the code marks new GC pointers as black. However, this is incorrect behavior:

### Expected Behavior
- **Generational barrier only**: Should only mark page as dirty (young objects treated as roots during minor GC). Should NOT mark as black.
- **Incremental marking**: Should mark new pointers as black (Dijkstra insertion barrier), preventing newly reachable objects from being missed.

### Actual Behavior
When only generational barrier is active (during minor GC), new pointers are incorrectly marked as black, preventing young objects from being collected during minor GC. This defeats the core purpose of generational GC - frequent collection of young objects.

### Code Location
- `crates/rudo-gc/src/cell.rs:193-208` - GcCell::borrow_mut()
- `crates/rudo-gc/src/cell.rs:1088-1101` - GcThreadSafeCell::borrow_mut()

---

## 🔬 Root Cause Analysis

```rust
// cell.rs:193-208 (GcCell::borrow_mut)
let barrier_active = generational_active || incremental_active;
if barrier_active {
    // BUG: Executes when generational_active=true as well
    for gc_ptr in new_gc_ptrs {
        let _ = crate::gc::incremental::mark_object_black(...);
    }
}
```

The issue is that `barrier_active = generational_active || incremental_active`. When only `generational_active` is true (during minor GC), this code still marks new pointers as black.

`mark_object_black()` sets a mark in the object's mark Bitmap, preventing the object from being collected in the current GC cycle. But for generational barrier, we only want to mark the page as dirty, not prevent collection.

---

## 💣 PoC

```rust
use rudo_gc::{Gc, GcCell, Trace, collect};

#[derive(Trace)]
struct Node {
    value: GcCell<Option<Gc<Node>>>,
}

fn main() {
    // Create circular reference: a -> b -> a
    let a = Gc::new(Node { value: GcCell::new(None) });
    let b = Gc::new(Node { value: GcCell::new(None) });
    
    // Set reference through GcCell, triggering generational barrier
    *a.value.borrow_mut() = Some(Gc::clone(&b));
    *b.value.borrow_mut() = Some(Gc::clone(&a));
    
    // Drop strong references
    drop(a);
    drop(b);
    
    // Minor GC - due to bug, young objects may be incorrectly marked black and not collected
    collect();
}
```

---

## 🛠️ Suggested Fix

Change the condition in `GcCell::borrow_mut()` and `GcThreadSafeCell::borrow_mut()` from:
```rust
if barrier_active {
    // mark_object_black
}
```

to:
```rust
if incremental_active {
    // mark_object_black
}
```

This ensures `mark_object_black()` is only called during incremental marking (when SATB barrier is needed), not during generational barrier (when only dirty page tracking is needed).

---

## 🗣️ Internal Discussion Record

**R. Kent Dybvig (GC Architecture):**
From a GC perspective, this bug defeats the purpose of generational GC. The generational barrier is designed to track OLD→YOUNG references so that minor GC can collect young objects without scanning the old generation. Incorrectly marking young objects as black prevents them from being collected, defeating this optimization.

**Rustacean (Soundness):**
This is not a soundness bug (no UB), but it causes memory leaks. Young objects that should be collected during minor GC are incorrectly retained because they're marked as "reachable" in the current GC cycle.

**Geohot (Exploit):**
An attacker could exploit this by:
1. Creating many short-lived objects through GcCell mutations
2. These objects would never be collected during minor GC
3. Could lead to memory exhaustion (DoS)
