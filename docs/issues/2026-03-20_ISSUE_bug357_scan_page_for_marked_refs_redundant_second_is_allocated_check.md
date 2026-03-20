# [Bug]: scan_page_for_marked_refs redundant second is_allocated check (bug258 fix now redundant)

**Status:** Open
**Tags:** Verified

## 📊 Threat Model Assessment

| Assessment | Rating | Description |
| :--- | :--- | :--- |
| **Likelihood** | Low | Code silently fails - redundant check provides no additional protection |
| **Severity** | Low | Does not cause memory corruption, just extra check |
| **Reproducibility** | N/A | Code structure issue, no reproduction needed |

---

## 🧩 Affected Component & Environment
- **Component:** `scan_page_for_marked_refs` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior

`scan_page_for_marked_refs` function should have one `is_allocated` check (to ensure slot is still valid after `try_mark`), followed by the generation check to detect slot reuse, then `push_work`.

### Actual Behavior

There are two consecutive `is_allocated` checks with NOTHING between them:

```rust
// First check (line 848)
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}
// NOTHING here - no code that could change slot state!
// Second check (line 854) - immediately follows first
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}
```

**Problem:**
1. Both checks are IDENTICAL
2. If first check passes (`is_allocated` returns true), second check MUST also pass
3. Second check is **dead code** - provides zero additional protection

---

## Root Cause Analysis

The second check was added as part of bug258's fix, which was intended to prevent TOCTOU between the first `is_allocated` check and `push_work`. However, the second check was placed IMMEDIATELY after the first, not before `push_work`.

The correct protection against slot reuse is the **generation check** at lines 861-865:
```rust
let current_generation = unsafe { (*gc_box_ptr).generation() };
if current_generation != marked_generation {
    (*header).clear_mark_atomic(i);
    break;
}
```

This generation check (bug336 fix) properly handles the case where:
- Slot is swept and reused between `try_mark` and checks
- If generations differ, the slot was reused and we skip

The second `is_allocated` check is now redundant because:
1. First check handles "slot swept and not reused" case
2. Generation check handles "slot swept and reused" case
3. Nothing between the two checks can change slot state

---

## Suggested Fix

Remove the redundant second `is_allocated` check:

```rust
Ok(true) => {
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    let marked_generation = unsafe { (*gc_box_ptr).generation() };

    // Re-check is_allocated to fix TOCTOU with lazy sweep (bug291).
    // If slot was swept after try_mark, clear mark and skip.
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        break;
    }
    
    // REMOVED: Redundant second is_allocated check (no longer needed)
    // The generation check below handles slot reuse detection.

    // Verify generation hasn't changed (bug336 fix).
    // If slot was reallocated between try_mark and push_work,
    // generation will differ and we should skip this object.
    let current_generation = unsafe { (*gc_box_ptr).generation() };
    if current_generation != marked_generation {
        (*header).clear_mark_atomic(i);
        break;
    }
    refs_found += 1;
    if let Some(gc_box) = NonNull::new(gc_box_ptr as *mut GcBox<()>) {
        state.push_work(gc_box);
    }
    break;
}
```

---

## Internal Discussion Record

**R. Kent Dybvig (GC Architecture):**
This looks like incorrect code copy-paste, or an over-fix of bug258. The first check already handles the TOCTOU after `try_mark`. The second check immediately follows the first with no intervening code, logically cannot see a different result.

**Rustacean (Soundness):**
Not a safety issue, just redundant check. Two consecutive identical checks, second can never have a different result.

**Geohot (Exploit):**
No actual attack surface. This erroneous check doesn't lead to any exploitable vulnerability.

---

## Related Issues

- bug258: Original issue documenting TOCTOU between is_allocated check and push_work
- bug291: Added first is_allocated re-check after try_mark
- bug336: Added generation check to detect slot reuse TOCTOU
- bug351: Similar issue in `scan_page_for_unmarked_refs` (different function)

---

## Fix Status

**Date:** 2026-03-20

**Issue created but fix NOT applied** - code still contains the redundant second `is_allocated` check at lines 854-857!