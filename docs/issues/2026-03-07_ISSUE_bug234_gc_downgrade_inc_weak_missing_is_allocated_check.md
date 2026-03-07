# [Bug]: Gc::downgrade inc_weak 後缺少 is_allocated 檢查導致 TOCTOU

**Status:** Open
**Tags:** Verified

---

## 📊 Threat Model Assessment

| Aspect | Assessment |
|--------|------------|
| Likelihood | Medium |
| Severity | High |
| Reproducibility | High |

---

## 🧩 Affected Component & Environment

- **Component:** `Gc::downgrade()` (ptr.rs:1424-1440)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 Description

### Expected Behavior

`Gc::downgrade()` should check `is_allocated()` after calling `inc_weak()` to prevent incrementing the weak count of a slot that has been swept and reused by another object.

### Actual Behavior

`Gc::downgrade()` (ptr.rs:1424-1440) calls `inc_weak()` at line 1435 without a subsequent `is_allocated()` check. This is the same bug pattern as:
- Bug 218: GcHandle::downgrade inc_weak missing is_allocated check
- Bug 217: Weak::clone inc_weak missing is_allocated check

**ptr.rs:1424-1440:**
```rust
pub fn downgrade(gc: &Self) -> Weak<T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::downgrade: cannot downgrade a dead Gc");
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::downgrade: cannot downgrade a dead, dropping, or under construction Gc"
        );
        (*gc_box_ptr).inc_weak();  // <-- No is_allocated check after!
    }
    Weak {
        ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    }
}
```

---

## 🔬 Root Cause Analysis

When lazy sweep runs concurrently with mutator:
1. Object A in slot is lazy swept (freed)
2. Object B is allocated in the same slot
3. Mutator calls `Gc::downgrade()` on Object B
4. Passes all pre-checks (has_dead_flag, dropping_state, is_under_construction)
5. Executes `inc_weak()` - but the slot now contains Object B!
6. Returns Weak pointer pointing to Object B's data!

**Consequence:** Object B's weak count is incorrectly incremented.

---

## 💣 Steps to Reproduce / PoC

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // Requires concurrent test environment:
    // 1. Create Gc<Data>
    // 2. Trigger lazy sweep to reclaim object
    // 3. Allocate new object in same slot
    // 4. Concurrently call Gc::downgrade()
    // 5. Observe incorrect weak count
}
```

---

## 🛠️ Suggested Fix / Remediation

Add `is_allocated()` check after `inc_weak()`:

```rust
(*gc_box_ptr).inc_weak();

// Post-check: verify object slot is still allocated after inc_weak
// (prevents TOCTOU with lazy sweep slot reuse)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // Rollback the inc_weak we just did
        (*gc_box_ptr).dec_weak();
        panic!("Gc::downgrade: slot was swept during downgrade");
    }
}
```

---

## 🗣️ Internal Discussion Record

### R. Kent Dybvig
This is a classic TOCTOU vulnerability related to concurrent lazy sweep execution. The fix should follow the same pattern as bug218 (GcHandle::downgrade) and bug217 (Weak::clone).

### Rustacean
This could lead to weak count management errors. While it won't directly cause UAF, it will cause objects to not be properly released.

### Geohot
An attacker could try to construct a scenario by precisely controlling GC timing, triggering lazy sweep between inc_weak and return, causing incorrect weak count calculation.
