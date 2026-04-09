# [Bug]: mark_object reads generation before is_allocated check - UB from deallocated slot access

**Status:** Open
**Tags:** Verified

## 📊 Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | Medium | Requires slot to be deallocated between initial is_allocated check and try_mark |
| **Severity** | High | UB from reading deallocated slot, stale mark may not be cleared |
| **Reproducibility** | Medium | Concurrent lazy sweep needed, stress tests can trigger |

---

## 🧩 Affected Component & Environment
- **Component:** `mark_object` (gc/gc.rs:2416-2427)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior

When `try_mark` succeeds and `is_allocated` re-check fails (slot was swept), the code should:
1. **Never** read `generation()` from a deallocated slot
2. **Always** clear the stale mark when slot is not allocated

### Actual Behavior

In `mark_object` (gc.rs:2416-2427):

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();  // line 2417 - READS GEN FIRST!
    if !(*header.as_ptr()).is_allocated(idx) {        // line 2418 - THEN CHECKS ALLOCATED
        // FIX bug554: Do NOT read generation from deallocated slot.
        // Slot was swept - ALWAYS clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    if (*ptr.as_ptr()).generation() != marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

**Problem:** At line 2417, `generation()` is read **BEFORE** the `is_allocated` check at line 2418. The slot could be deallocated between:
1. Initial `is_allocated` check at line 2404 (before loop)
2. `try_mark` succeeds at line 2409
3. `generation()` read at line 2417

If the slot is deallocated before line 2417, we read `generation()` from deallocated memory - **undefined behavior**.

**Additional issue:** Even if generation is read from valid memory, if the slot was swept (not reused) between line 2404 and line 2417, `marked_generation` would equal the current generation, and the slot would incorrectly retain its stale mark.

---

## Root Cause Analysis

**Race Window:**
1. Initial `is_allocated` check passes at line 2404
2. Thread enters loop at line 2408
3. `try_mark` succeeds at line 2409
4. **Race window**: Another thread's lazy sweep deallocates the slot
5. `marked_generation` captured at line 2417 from deallocated slot - **UB**
6. `is_allocated` check at line 2418 fails
7. Mark is cleared, but UB has already occurred

**Why this is different from `mark_and_trace_incremental`:**
In `mark_and_trace_incremental` (lines 2471-2478), the fix was applied:
```rust
Ok(true) => {
    // FIX bug557: Check is_allocated FIRST to avoid UB.
    // Reading generation from a deallocated slot is undefined behavior.
    if !(*header.as_ptr()).is_allocated(idx) {
        // FIX bug552: Slot was swept - ALWAYS clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // Now safe to read generation from guaranteed allocated slot
    let marked_generation = (*ptr.as_ptr()).generation();
```

But in `mark_object`, the same fix was NOT applied - generation is still read before the allocated check.

---

## PoC

```rust
// Requires concurrent lazy sweep:
// 1. Allocate object A in slot
// 2. Initial is_allocated check passes (slot allocated)
// 3. try_mark succeeds on object A
// 4. Lazy sweep deallocates slot (object A collected)
// 5. marked_generation read from deallocated slot - UB!
```

---

## Suggested Fix

Apply the same pattern from `mark_and_trace_incremental` to `mark_object`:

```rust
Ok(true) => {
    // FIX bug559: Check is_allocated FIRST to avoid UB.
    // Reading generation from a deallocated slot is undefined behavior.
    if !(*header.as_ptr()).is_allocated(idx) {
        // Slot was swept - ALWAYS clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // Now safe to read generation from guaranteed allocated slot
    let marked_generation = (*ptr.as_ptr()).generation();
    if (*ptr.as_ptr()).generation() != marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

---

## Internal Discussion Record

**R. Kent Dybvig (GC Architecture Perspective):**
The generation check is for detecting slot REUSE, not for protecting against reading deallocated memory. When `is_allocated` is false, we must not read any GcBox fields - the slot is dead and the memory may be reclaimed.

**Rustacean (Soundness Perspective):**
Reading from deallocated memory is undefined behavior in Rust. The code must check `is_allocated` BEFORE reading any GcBox fields including `generation()`. This is a soundness issue.

**Geohot (Exploit Perspective):**
An attacker could potentially manipulate GC timing to cause stale marks to persist, leading to memory exhaustion. Combined with the UB from reading deallocated memory, this is a serious correctness issue.

---

## Related Issues

- bug554: mark_object reads gen from deallocated slot (Status: Open, but claims Fixed in header)
- bug557: mark_and_trace_incremental fixed correctly - same pattern
- bug551: mark_object_minor same bug (Status: Fixed)