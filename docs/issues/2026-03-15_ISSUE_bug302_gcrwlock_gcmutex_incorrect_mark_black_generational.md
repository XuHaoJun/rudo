# [Bug]: GcRwLock/GcMutex incorrectly marks new GC pointers black during generational barrier

**Status:** Open
**Tags:** Unverified

## 📊 Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | High | Triggered every time GcRwLock/GcMutex write methods are used when generational barrier is active |
| **Severity** | High | Causes young objects to not be collected during minor GC, leading to memory leaks |
| **Reproducibility** | Low | Need minor GC + GcRwLock/GcMutex mutation |

---

## 🧩 Affected Component & Environment
- **Component:** `GcRwLock::write()`, `GcRwLock::try_write()`, `GcMutex::lock()`, `GcMutex::try_lock()` in `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current (unfixed)

---

## 📝 Description

This is the same bug pattern as bug301, but in `sync.rs` instead of `cell.rs`.

In `GcRwLock::write()`, `GcRwLock::try_write()`, `GcMutex::lock()`, and `GcMutex::try_lock()`, when `barrier_active` (= `generational_active || incremental_active`) is true, the code marks new GC pointers as black via `mark_gc_ptrs_immediate()`. However, this is incorrect behavior:

### Expected Behavior
- **Generational barrier only**: Should only trigger write barrier (mark page as dirty). Should NOT mark new pointers as black.
- **Incremental marking**: Should mark new pointers as black (Dijkstra insertion barrier), preventing newly reachable objects from being missed.

### Actual Behavior
When only generational barrier is active (during minor GC), new pointers are incorrectly marked as black, preventing young objects from being collected during minor GC. This defeats the core purpose of generational GC - frequent collection of young objects.

### Code Location
- `crates/rudo-gc/src/sync.rs:288` - GcRwLock::write()
- `crates/rudo-gc/src/sync.rs:330` - GcRwLock::try_write()
- `crates/rudo-gc/src/sync.rs:588` - GcMutex::lock()
- `crates/rudo-gc/src/sync.rs:628` - GcMutex::try_lock()

---

## 🔬 Root Cause Analysis

```rust
// sync.rs:288 (GcRwLock::write)
let barrier_active = generational_active || incremental_active;
mark_gc_ptrs_immediate(&*guard, barrier_active);  // BUG: should use incremental_active only
```

The issue is that `mark_gc_ptrs_immediate()` is called with `barrier_active = generational_active || incremental_active`. When only `generational_active` is true (during minor GC), this code still marks new pointers as black.

`mark_object_black()` sets a mark in the object's mark Bitmap, preventing the object from being collected in the current GC cycle. But for generational barrier, we only want to mark the page as dirty, not prevent collection.

---

## 💣 PoC

```rust
use rudo_gc::{Gc, GcRwLock, Trace, collect};

#[derive(Trace)]
struct Node {
    value: GcRwLock<Option<Gc<Node>>>,
}

fn main() {
    // Create circular reference: a -> b -> a
    let a = Gc::new(Node { value: GcRwLock::new(None) });
    let b = Gc::new(Node { value: GcRwLock::new(None) });
    
    // Set reference through GcRwLock, triggering generational barrier
    *a.value.write() = Some(Gc::clone(&b));
    *b.value.write() = Some(Gc::clone(&a));
    
    // Drop strong references
    drop(a);
    drop(b);
    
    // Minor GC - due to bug, young objects may be incorrectly marked black and not collected
    collect();
}
```

---

## 🛠️ Suggested Fix

Change the condition in `GcRwLock::write()`, `GcRwLock::try_write()`, `GcMutex::lock()`, and `GcMutex::try_lock()` from:
```rust
let barrier_active = generational_active || incremental_active;
mark_gc_ptrs_immediate(&*guard, barrier_active);
```

to:
```rust
mark_gc_ptrs_immediate(&*guard, incremental_active);
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
1. Creating many short-lived objects through GcRwLock/GcMutex mutations
2. These objects would never be collected during minor GC
3. Could lead to memory exhaustion (DoS)

---

## 📌 Related Issues

- bug301: GcCell::borrow_mut() incorrectly marks new GC pointers black during generational barrier (same bug pattern, different location)
