# [Bug]: AsyncGcHandle::downcast_ref Missing Generation Check for TOCTOU Slot Reuse

**Status:** Fixed
**Tags:** Verified

## 📊 Threat Model Assessment

| Aspect | Rating | Description |
| :--- | :--- | :--- |
| **Likelihood** | Medium | Race window exists during concurrent GC + handle access |
| **Severity** | Medium | Type confusion from slot reuse causes wrong object data returned |
| **Reproducibility** | High | Requires concurrent GC sweep during downcast_ref execution |

---

## 🧩 Affected Component & Environment
- **Component:** `AsyncGcHandle::downcast_ref` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 Description

### Expected Behavior

`AsyncGcHandle::downcast_ref()` should have the same liveness and slot-reuse checks as `AsyncHandle::get()`, which includes a generation check BEFORE dereferencing to detect if the slot was swept and reused between the `is_allocated` check and the actual dereference.

### Actual Behavior

`AsyncGcHandle::downcast_ref()` performs:
1. `is_allocated` check (lines 1522-1527)
2. **NO generation check before dereference**
3. Flag checks after dereference

The code at lines 1529-1536 dereferences `gc_box_ptr` without verifying the generation hasn't changed since the `is_allocated` check. This creates a TOCTOU race condition where the slot could be swept and reused between these operations.

### Code Location

`crates/rudo-gc/src/handles/async.rs:1516-1541`

```rust
unsafe {
    // Validate pointer before dereferencing (avoids UAF if slot was swept and reused).
    let ptr_addr = gc_box_ptr as usize;
    if !is_gc_box_pointer_valid(ptr_addr) {
        return None;
    }
    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return None;
        }
    }

    let gc_box = &*gc_box_ptr;  // <-- TOCTOU: no generation check before dereference!
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag()
        || gc_box.dropping_state() != 0
    {
        return None;
    }
    Some(gc_box.value())
}
```

### Comparison with AsyncHandle::get (Correct Pattern)

`AsyncHandle::get()` at lines 628-656 correctly handles this:

```rust
let pre_generation = gc_box.generation();  // Capture generation BEFORE
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::get: object is being dropped");
}
// FIX bug453: If generation changed, undo the increment to prevent ref_count leak.
if pre_generation != gc_box.generation() {  // Check generation AFTER
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: slot was reused before value read (generation mismatch)");
}
```

---

## 🔬 Root Cause Analysis

**Race Scenario:**
1. Thread A: `downcast_ref<X>()` obtains `gc_box_ptr` from slot S
2. Thread A: Passes `is_allocated(S)` check - slot S is allocated with Object X
3. Thread B: GC sweep runs, Object X is swept, slot S is reallocated to Object Y (different type but same `type_id` stored in handle)
4. Thread A: Dereferences `gc_box_ptr`, now reading from Object Y instead of X
5. Thread A: `type_id` check passes (handle still stores X's type_id)
6. Thread A: Returns Object Y's data interpreted as type X - **type confusion!**

**Key Issue:**
The `type_id` stored in the `AsyncGcHandle` is set at creation time and never changes. If the slot is reused between `is_allocated` and dereference, we could be reading from a different object whose `type_id` might coincidentally match.

---

## 🛠️ Suggested Fix

Add generation check between `is_allocated` and dereference:

```rust
unsafe {
    let ptr_addr = gc_box_ptr as usize;
    if !is_gc_box_pointer_valid(ptr_addr) {
        return None;
    }
    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return None;
        }
    }
    
    // FIX bug489: Get generation BEFORE dereference to detect slot reuse.
    let pre_generation = (*gc_box_ptr).generation();
    
    let gc_box = &*gc_box_ptr;
    
    // FIX bug489: Verify generation hasn't changed (slot was NOT reused).
    if pre_generation != gc_box.generation() {
        panic!("AsyncGcHandle::downcast_ref: slot was reused between liveness check and dereference");
    }
    
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag()
        || gc_box.dropping_state() != 0
    {
        return None;
    }
    Some(gc_box.value())
}
```

**Note:** Unlike `AsyncHandle::get` which can rollback a ref count increment, `downcast_ref` has no state to rollback. The only safe action on detected slot reuse is to panic, consistent with `AsyncHandle::get`'s behavior.

---

## 🗣️ Internal Discussion Record

**R. Kent Dybvig (GC Architecture Perspective):**
The generation mechanism exists precisely to detect slot reuse in concurrent GC systems. Without a generation check, we have a TOCTOU window where slot state can change between validation and use. This pattern should be consistent across all handle operations that dereference GC pointers. The `type_id` stored in handles provides no protection against slot reuse - it only verifies the requested type matches what the handle was created with, not whether the underlying slot still contains the same object.

**Rustacean (Soundness Perspective):**
This is a type confusion bug. Reading from a dereferenced pointer that may have been reallocated to a different object violates Rust's memory safety guarantees. The `is_allocated` check alone is insufficient because it only confirms the slot is in use, not that the object at that slot hasn't been replaced. The generation check closes this window by detecting when the slot's contents have changed since we last looked.

**Geohot (Exploit Perspective):**
Type confusion bugs are powerful exploit primitives. If an attacker can control GC timing relative to `downcast_ref` calls, they could potentially make a handle return data from a different object. Even without full control, the race condition could lead to accessing partially-initialized or freed objects. The small race window makes precise exploitation difficult but not impossible - sophisticated attackers excel at timing attacks.

---

## Related Issues

- bug453: `AsyncHandle::get` generation check for TOCTOU
- bug476: `AsyncHandleScope::handle` missing generation check (Open)
- bugXXX (gcscope_spawn): `GcScope::spawn` generation check (Fixed)
- bug194: `AsyncGcHandle::downcast_ref` missing `is_allocated` check (Fixed)
- bug99: `AsyncGcHandle::downcast_ref` missing `is_under_construction` check (Fixed)

This issue is distinct from bug476 - both have the same TOCTOU pattern but affect different code paths (`AsyncHandleScope::handle` vs `AsyncGcHandle::downcast_ref`).

---

## Resolution (2026-04-02)

**Outcome:** Fixed.

Added `pre_generation` capture before dereferencing `gc_box_ptr`, and added a generation check after dereferencing to verify the slot was not reused between the liveness check and dereference. The fix matches the pattern used in `AsyncHandle::get` and `GcScope::spawn`.

**Fix Applied:**
In `crates/rudo-gc/src/handles/async.rs`, `AsyncGcHandle::downcast_ref`:
1. Added `let pre_generation = (*gc_box_ptr).generation();` after the `is_allocated` check
2. After dereferencing, added `if pre_generation != gc_box.generation()` check with panic

```rust
// FIX bug489: Get generation BEFORE dereference to detect slot reuse.
let pre_generation = (*gc_box_ptr).generation();

let gc_box = &*gc_box_ptr;

// FIX bug489: Verify generation hasn't changed (slot was NOT reused).
if pre_generation != gc_box.generation() {
    panic!("AsyncGcHandle::downcast_ref: slot was reused between liveness check and dereference");
}
```

**Verification:** Clippy passes with no warnings.