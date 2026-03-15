# [Bug]: GcRwLockWriteGuard and GcMutexGuard Drop implementations incorrectly mark GC pointers black during generational barrier

**Status:** Verified
**Tags:** Verified

## 📊 Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | High | Triggered every time GcRwLock/GcMutex write guards are dropped when generational barrier is active |
| **Severity** | High | Causes young objects to not be collected during minor GC, leading to memory leaks |
| **Reproducibility** | Low | Need minor GC + GcRwLock/GcMutex mutation + guard drop |

---

## 🧩 Affected Component & Environment
- **Component:** `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()` in `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current (unfixed)

---

## 📝 Description

This bug is an extension of bug302, which was already reported but only covered the `write()`/`try_write()`/`lock()`/`try_lock()` methods. The **Drop implementations for write guards have the SAME bug but were not explicitly mentioned in bug302**.

In `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()`, when either `incremental_active` OR `generational_active` is true, the code marks new GC pointers as black via `mark_object_black()`. However, this is incorrect behavior:

### Expected Behavior
- **Generational barrier only**: Should only mark page as dirty. Should NOT mark new pointers as black.
- **Incremental marking**: Should mark new pointers as black (Dijkstra insertion barrier), preventing newly reachable objects from being missed.

### Actual Behavior
When only generational barrier is active (during minor GC), new pointers are incorrectly marked as black, preventing young objects from being collected during minor GC. This defeats the core purpose of generational GC - frequent collection of young objects.

### Code Location
- `crates/rudo-gc/src/sync.rs:466` - GcRwLockWriteGuard::drop()
- `crates/rudo-gc/src/sync.rs:733` - GcMutexGuard::drop()

---

## 🔬 Root Cause Analysis

```rust
// sync.rs:466 (GcRwLockWriteGuard::drop)
if incremental_active || generational_active {  // BUG: should be incremental_active only
    for gc_ptr in &ptrs {
        let _ = unsafe {
            crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
        };
    }
}
```

The issue is that `mark_object_black()` is called when `incremental_active || generational_active` is true. When only `generational_active` is true (during minor GC), this code still marks new pointers as black.

Note: There's a misleading comment at lines 733-736 in GcMutexGuard::drop() that says:
```rust
// Always mark when we have ptrs to eliminate TOCTOU: barrier state may change between
// any check and mark. mark_object_black is idempotent and safe when barrier is inactive;
```

This reasoning is incorrect because the issue is NOT about TOCTOU or safety - it's about SEMANTIC CORRECTNESS. Marking objects black during generational barrier prevents them from being collected during minor GC, defeating the purpose of generational GC.

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
    // Then drop the guard (which triggers Drop)
    {
        let mut guard = a.value.write();
        *guard = Some(Gc::clone(&b));
    } // Guard dropped here - bug is in Drop implementation
    
    {
        let mut guard = b.value.write();
        *guard = Some(Gc::clone(&a));
    }
    
    // Drop strong references
    drop(a);
    drop(b);
    
    // Minor GC - due to bug, young objects may be incorrectly marked black and not collected
    collect();
}
```

---

## 🛠️ Suggested Fix

Change the condition in `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()` from:
```rust
if incremental_active || generational_active {
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
From a GC perspective, this bug defeats the purpose of generational GC. The generational barrier is designed to track OLD→YOUNG references so that minor GC can collect young objects without scanning the old generation. Incorrectly marking young objects as black prevents them from being collected, defeating this optimization. The comment in GcMutexGuard::drop() is misleading - it's not about TOCTOU or idempotency, it's about semantic correctness of generational GC.

**Rustacean (Soundness):**
This is not a soundness bug (no UB), but it causes memory leaks. Young objects that should be collected during minor GC are incorrectly retained because they're marked as "reachable" in the current GC cycle.

**Geohot (Exploit):**
An attacker could exploit this by:
1. Creating many short-lived objects through GcRwLock/GcMutex mutations
2. These objects would never be collected during minor GC
3. Could lead to memory exhaustion (DoS)

---

## 📌 Related Issues

- bug302: GcRwLock/GcMutex incorrectly marks new GC pointers black during generational barrier (same bug pattern, but only covers write()/try_write()/lock()/try_lock() methods, NOT Drop implementations)
- bug301: GcCell::borrow_mut() incorrectly marks new GC pointers black during generational barrier (same bug pattern in cell.rs)
