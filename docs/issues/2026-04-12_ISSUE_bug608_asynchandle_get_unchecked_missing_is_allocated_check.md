# [Bug]: AsyncHandle::get_unchecked missing is_allocated check after undo block

**Status:** Open
**Tags:** Unverified

## Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | Low | Requires slot to be swept between undo block and value() call |
| **Severity** | Medium | Could read from deallocated slot, causing memory corruption |
| **Reproducibility** | Medium | Requires precise timing with lazy sweep |

---

## Affected Component & Environment

- **Component:** `AsyncHandle::get_unchecked()` in `handles/async.rs:738-798`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior
`AsyncHandle::get_unchecked()` should have the same defensive `is_allocated` checks as `Handle::get()`, including an `is_allocated` check after the dead/dropping/uac undo block and before `value()`.

### Actual Behavior
`AsyncHandle::get_unchecked()` has only ONE `is_allocated` check (lines 773-779) BEFORE the dead/dropping/uac undo block (lines 781-788), but is MISSING the second `is_allocated` check that `Handle::get()` has after its undo block.

### Comparison with Handle::get()

`Handle::get()` (handles/mod.rs:302-372) has this pattern:
```
1. is_allocated check (lines 310-316)
2. dead/dropping/uac checks (lines 318-323)
3. pre_generation (line 324)
4. try_inc_ref_if_nonzero (lines 325-327)
5. generation check + undo if mismatch (lines 328-331)
6. dead/dropping/uac check + undo (lines 333-339)
7. SECOND is_allocated check (lines 352-358) ← AFTER undo block
8. dead/dropping/uac RE-CHECK (lines 362-367) ← catches if object became dead after undo
9. value() (line 369)
```

But `AsyncHandle::get_unchecked()` (handles/async.rs:738-798) has:
```
1. is_allocated check (lines 749-755)
2. dead/dropping/uac checks (lines 757-762)
3. pre_generation (line 763)
4. try_inc_ref_if_nonzero (lines 764-766)
5. generation check + undo if mismatch (lines 767-771)
6. is_allocated check (lines 773-779) ← BEFORE undo block
7. dead/dropping/uac check + undo (lines 781-788)
8. MISSING: SECOND is_allocated check after undo block ← BUG!
9. value() (line 796)
```

The gap is at step 8: `AsyncHandle::get_unchecked()` does NOT have the second `is_allocated` check that `Handle::get()` has after its undo block.

---

## Root Cause Analysis

The bug is in `handles/async.rs:781-796`. The `is_allocated` check is positioned BEFORE the undo block instead of AFTER it.

**The vulnerable sequence:**
1. `AsyncHandle::get_unchecked()` calls `try_inc_ref_if_nonzero()` which succeeds
2. Generation check passes (pre == current)
3. First `is_allocated` check passes at lines 773-779 - slot is still allocated
4. Dead/dropping/uac check passes at lines 781-788 - we don't undo
5. **Lazy sweep could run HERE and sweep this slot**
6. `value()` is called on deallocated memory → **Undefined Behavior**

The generation check cannot catch this because generation only changes when a slot is REUSED, not when it's simply SWEPT. If the slot is swept without reuse, generation remains the same.

**Note:** This bug is similar to bug607 but affects `get_unchecked()` instead of `get()`. `AsyncHandle::get()` already has the fix at lines 690-696.

---

## Proof of Concept

```rust
// Pseudocode showing the vulnerable sequence in AsyncHandle::get_unchecked()
// (simplified from handles/async.rs:738-798)

unsafe fn get_unchecked(&self) -> &T {
    let gc_box_ptr = /* obtained from slot */;
    
    // First is_allocated check (lines 773-779)
    if let Some(idx) = ptr_to_object_index(gc_box_ptr) {
        let header = ptr_to_page_header(gc_box_ptr);
        assert!((*header).is_allocated(idx)); // PASSES
    }
    
    let gc_box = &*gc_box_ptr;
    
    // ... generation checks ...
    
    // undo block (lines 781-788)
    if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 || gc_box.is_under_construction() {
        undo_inc_ref();
        panic!();
    }
    // We pass through here since object is alive
    
    // BUG: NO second is_allocated check here!
    
    // Meanwhile, lazy sweep could run here and sweep this slot!
    // Generation wouldn't change (no reuse), so we wouldn't catch it.
    
    let value = gc_box.value(); // ← Could read deallocated memory!
    value
}
```

Compare with `Handle::get()` which has the second check after the undo block:
```rust
// From handles/mod.rs:352-358
// Second is_allocated check after undo block to fix TOCTOU with lazy sweep.
if let Some(idx) = ptr_to_object_index(gc_box_ptr) {
    let header = ptr_to_page_header(gc_box_ptr);
    assert!((*header).is_allocated(idx), "Handle::get: object slot was swept");
}
```

---

## Suggested Fix

Add a second `is_allocated` check in `AsyncHandle::get_unchecked()` after the dead/dropping/uac undo block and before `value()`:

```rust
// In handles/async.rs, after line 788 (after the undo block in AsyncHandle::get_unchecked())

// FIX bug608: Add second is_allocated check after undo block.
// This matches the pattern in Handle::get() and prevents TOCTOU where
// lazy sweep could reclaim the slot between the undo block and value() call.
if let Some(idx) = unsafe { crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) } {
    let header = unsafe { crate::heap::ptr_to_page_header(gc_box_ptr as *const u8) };
    assert!(
        unsafe { (*header.as_ptr()).is_allocated(idx) },
        "AsyncHandle::get_unchecked: object slot was swept after undo block"
    );
}

let value = gc_box.value();
value
```

---

## Internal Discussion

**R. Kent Dybvig (GC architecture perspective):**
The incremental marking and lazy sweep infrastructure creates a window where a slot can be reclaimed between the undo block and the final value access. This is a classic TOCTOU (time-of-check-time-of-use) race. The key insight is that the generation check only catches slot REUSE (generation changes), not slot SWEEPING (generation stays same). An is_allocated check after the undo block is needed to catch the sweeping case.

**Rustacean (Soundness/UB perspective):**
This is a genuine memory safety issue. Reading from a deallocated slot could cause:
1. UAF (use-after-free) if the slot was reclaimed
2. Type confusion if the slot was reallocated to a different type
The fix is straightforward - add the missing second is_allocated check after the undo block.

**George Hotz (Exploit perspective):**
While the race condition is hard to trigger reliably, an attacker who can control GC timing (e.g., through allocation patterns) could potentially exploit this for information disclosure or to cause crashes. The defensive fix should be applied regardless of difficulty of exploitation.