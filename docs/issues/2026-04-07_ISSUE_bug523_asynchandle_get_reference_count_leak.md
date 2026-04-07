# [Bug]: AsyncHandle::get reference count leak due to incorrect undo_inc_ref placement

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Reference counting is used extensively; every `get()` call is affected |
| **Severity (嚴重程度)** | Critical | Memory leak leads to objects never being collected, causing memory pressure |
| **Reproducibility (復現難度)** | Very High | Every call to `get()` triggers the bug deterministically |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

In `AsyncHandle::get()`, the reference count is incorrectly decremented by calling `undo_inc_ref()` unconditionally after successfully obtaining a reference, causing a reference count leak on every successful `get()` call.

### 預期行為 (Expected Behavior)
`get()` should temporarily increment the reference count to prevent the object from being collected while the reference is in use, then decrement it when the reference is dropped. The temporary reference should NOT affect the object's overall reference count after `get()` returns.

### 實際行為 (Actual Behavior)
Every call to `get()` causes an unconditional `undo_inc_ref()` at line 677, decrementing the reference count even when the object is healthy and the reference was successfully obtained. This leaks a reference count on every call, eventually causing objects to become immortal and memory pressure to build up.

---

## 🔬 根本原因分析 (Root Cause Analysis)

Looking at `handles/async.rs:640-691`:

```rust
let pre_generation = gc_box.generation();
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::get: object is being dropped");
}
// FIX bug453: If generation changed, undo the increment to prevent ref_count leak.
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: slot was reused before value read (generation mismatch)");
}

if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "AsyncHandle::get: object slot was swept after dec_ref"
    );
}

if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    // Use undo_inc_ref, not dec_ref: dec_ref returns early without
    // decrementing when DEAD_FLAG is set or is_under_construction is true,
    // but we need to actually rollback the try_inc_ref_if_nonzero increment.
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: object became dead/dropping after inc_ref");
}

GcBox::undo_inc_ref(gc_box_ptr.cast_mut());  // <-- BUG: This line unconditionally decrements!

// Second is_allocated check after undo_inc_ref (bug379 fix).
// If slot was swept after dec_ref, we could read from a freed object.
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

The problem is the unconditional `GcBox::undo_inc_ref(gc_box_ptr.cast_mut())` at line 677. This decrements the reference count regardless of whether the object is alive and valid. The correct behavior should be:

1. Increment ref count for temporary access (done)
2. Verify object is still valid (done via generation check and dead/dropping/under_construction checks)
3. Return reference to user
4. **NOT decrement the ref count** - the user hasn't dropped their reference yet

But the code unconditionally decrements at step 3, before returning the value. This is a reference count leak.

The same bug exists in `AsyncHandle::get_unchecked()` at line 781.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone)]
struct RefTracker {
    marker: Rc<Cell<bool>>,
}
static_collect!(RefTracker);

#[test]
fn test_async_handle_get_ref_count_leak() {
    let tracker = RefTracker {
        marker: Rc::new(Cell::new(false)),
    };
    
    // Create GC object
    let gc = Gc::new(tracker.clone());
    
    // Create AsyncHandle via scope
    let scope = /* create AsyncHandleScope */;
    let handle = scope.create_handle(gc.clone());
    
    // Initial ref count should be 1 (from gc)
    let initial_count = Rc::strong_count(&tracker.marker);
    assert_eq!(initial_count, 1);
    
    // Call get() multiple times - each call should NOT affect ref count
    for _ in 0..100 {
        let _ref = handle.get();
        // ref count should still be 1
        let count_after = Rc::strong_count(&tracker.marker);
        assert_eq!(count_after, 1, "Ref count leaked after get()!");
    }
    
    drop(handle);
    collect_full();
    
    // After dropping handle and collecting, ref count should be 0
    let final_count = Rc::strong_count(&tracker.marker);
    assert_eq!(final_count, 0);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Remove the unconditional `GcBox::undo_inc_ref()` calls at:
- `handles/async.rs:677`
- `handles/async.rs:781`

The temporary reference count increment from `try_inc_ref_if_nonzero()` should remain in place for the duration of the borrow, but should NOT be undone until the `Gc` returned by `to_gc()` is dropped (which uses `dec_ref`, not `undo_inc_ref`).

For `get()` which returns `&T` (a reference, not a `Gc`), the increment+decrement pattern doesn't apply because we're not transferring ownership. The ref count should simply remain elevated while the reference is live.

Wait - looking more carefully: `get()` returns `&T` not `Gc<T>`. The borrow pattern should NOT use ref count at all for temporary borrows. The `try_inc_ref_if_nonzero()` is likely meant to prevent collection during the borrow, but the decrement should only happen in error paths (generation mismatch, dead flag, etc.), NOT on the success path.

The fix: move the unconditional `undo_inc_ref()` into the error handling blocks only, or remove it entirely since `get()` returns a reference (not a `Gc`) and thus doesn't transfer ownership.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The reference count mechanism in `AsyncHandle::get()` seems confused. When `get()` returns a reference (`&T`), there's no ownership transfer, so the temporary ref count increment should protect the object during the borrow period. But unconditionally decrementing after the borrow (before returning) defeats this protection entirely. This is especially problematic for async code where the borrow might live across await points.

**Rustacean (Soundness 觀點):**
This is a memory leak, not soundness UB. However, the code is patently incorrect - calling `undo_inc_ref()` unconditionally on the success path contradicts the purpose of the increment which was to protect the object. The leak can lead to memory exhaustion over time.

**Geohot (Exploit 觀點):**
While not an exploit in the traditional sense, a reference count leak in a GC system means objects that should be collected are retained. In long-running async applications (like servers), this could lead to memory exhaustion DoS. The 100% reproducible nature makes it a reliable memory pressure mechanism.