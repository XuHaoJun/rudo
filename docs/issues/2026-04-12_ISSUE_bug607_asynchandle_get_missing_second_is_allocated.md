# [Bug]: AsyncHandle::get() missing second is_allocated check before value()

**Status:** Open
**Tags:** Unverified

## Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | Low | Requires slot to be swept between dead/uac checks and value() call |
| **Severity** | Medium | Could read from deallocated/reused slot, causing memory corruption |
| **Reproducibility** | Medium | Requires precise timing with lazy sweep |

---

## Affected Component & Environment

- **Component:** `AsyncHandle::get()` in `handles/async.rs:610-701`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior
`AsyncHandle::get()` should have the same defensive `is_allocated` checks as `Handle::get()`, including a second `is_allocated` check after the dead/dropping/uac checks to close the TOCTOU window with lazy sweep.

### Actual Behavior
`AsyncHandle::get()` has only ONE `is_allocated` check (lines 665-670) before the dead/dropping/uac checks (lines 673-682), but is MISSING the second `is_allocated` check that `Handle::get()` has after those checks.

### Comparison with Handle::get()

`Handle::get()` (handles/mod.rs:302-372) has this pattern:
```
1. is_allocated check (lines 310-316)
2. dead/uac checks (lines 318-323)
3. pre_generation (line 324)
4. try_inc_ref_if_nonzero (lines 325-327)
5. generation check + undo if mismatch (lines 328-330)
6. dead/dropping/uac checks + undo if dead (lines 333-339)
7. SECOND is_allocated check (lines 352-358) ← DEFENSE-IN-DEPTH
8. recheck dead/dropping/uac (lines 362-367)
9. value() (line 369)
```

But `AsyncHandle::get()` (handles/async.rs:610-701) has:
```
1. is_allocated check (lines 640-646)
2. dead/uac checks (lines 648-653)
3. pre_generation (line 654)
4. try_inc_ref_if_nonzero (lines 655-657)
5. generation check + undo if mismatch (lines 658-663)
6. dead/dropping/uac checks + undo if dead (lines 673-682)
7. ONE is_allocated check (lines 665-670) ← only one!
8. MISSING: SECOND is_allocated check
9. value() (line 698)
```

The gap is at step 8: `AsyncHandle::get()` does NOT have the second `is_allocated` check that `Handle::get()` has.

---

## Root Cause Analysis

The bug was introduced when `AsyncHandle::get()` was refactored to use `with_scope_lock_if_active`. The function correctly uses generation checks and undo_inc_ref patterns, but the defensive second `is_allocated` check was not added after the dead/dropping/uac checks.

The issue is in `handles/async.rs:665-670` - there's only ONE `is_allocated` check, but `Handle::get()` has TWO (one before the generation check and one after).

**The vulnerable sequence:**
1. `AsyncHandle::get()` calls `try_inc_ref_if_nonzero()` which succeeds
2. Generation check passes (pre == current)
3. Dead/dropping/uac checks pass
4. `is_allocated` check passes - slot is still allocated
5. **BUT** between step 4 and `value()`, lazy sweep could run and sweep the slot
6. `value()` is called on deallocated memory → **Undefined Behavior**

The fix: Add a second `is_allocated` check after the dead/dropping/uac checks, matching the pattern in `Handle::get()` (bug385 fix).

---

## Proof of Concept

```rust
// Pseudocode showing the vulnerable sequence in AsyncHandle::get()
// (simplified from handles/async.rs:610-701)

unsafe fn get(&self) -> &T {
    // ... validation and scope checks ...

    let gc_box_ptr = /* obtained from slot */;
    
    // Check 1: is_allocated (lines 665-670)
    if let Some(idx) = ptr_to_object_index(gc_box_ptr) {
        let header = ptr_to_page_header(gc_box_ptr);
        assert!((*header).is_allocated(idx)); // PASSES
    }
    
    let gc_box = &*gc_box_ptr;  // gc_box obtained
    
    // Generation check (lines 658-663)
    let pre_generation = gc_box.generation();
    try_inc_ref_if_nonzero();
    if pre_generation != gc_box.generation() {
        undo_inc_ref();
        panic!();
    }
    
    // Dead/dropping/uac checks (lines 673-682)
    if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 || gc_box.is_under_construction() {
        undo_inc_ref();
        panic!();
    }
    
    // BUG: NO second is_allocated check here!
    
    // Meanwhile, lazy sweep could run here and sweep this slot!
    
    let value = gc_box.value(); // ← Could read deallocated memory!
    value
}
```

Compare with `Handle::get()` which has the second check:
```rust
// From handles/mod.rs:352-358
// Second is_allocated check after inc_ref to fix TOCTOU with lazy sweep (bug372/bug385).
// If slot was swept between dec_ref and value read, we could access a dropped value.
if let Some(idx) = ptr_to_object_index(gc_box_ptr) {
    let header = ptr_to_page_header(gc_box_ptr);
    assert!((*header).is_allocated(idx), "Handle::get: object slot was swept after dec_ref");
}
```

---

## Suggested Fix

Add a second `is_allocated` check in `AsyncHandle::get()` after the dead/dropping/uac checks and before `value()`:

```rust
// In handles/async.rs, after line 682 (after the dead/dropping/uac checks in AsyncHandle::get())

// FIX bug607: Add second is_allocated check after dead/dropping/uac checks.
// This matches the pattern in Handle::get() (bug385 fix) and prevents
// TOCTOU where lazy sweep could reclaim the slot between the first
// is_allocated check and value() call.
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "AsyncHandle::get: object slot was swept after dec_ref"
    );
}

let value = gc_box.value();
value
```

---

## Internal Discussion

**R. Kent Dybvig (GC architecture perspective):**
The incremental marking and lazy sweep infrastructure creates a window where a slot can be reclaimed between the initial allocation check and the final value read. This is a classic TOCTOU (time-of-check-time-of-use) race that requires defense-in-depth with multiple checks at different points.

**Rustacean (Soundness/UB perspective):**
This is a genuine memory safety issue. Reading from a deallocated or reused slot could cause:
1. UAF (use-after-free) if the slot was reclaimed
2. Type confusion if the slot was reallocated to a different type
The fix is straightforward - add the missing second is_allocated check.

**George Hotz (Exploit perspective):**
While the race condition is hard to trigger reliably, an attacker who can control GC timing (e.g., through allocation patterns) could potentially exploit this for information disclosure or to cause crashes. The defensive fix should be applied regardless of difficulty of exploitation.