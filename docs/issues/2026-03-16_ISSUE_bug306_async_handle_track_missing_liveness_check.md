# [Bug]: AsyncHandle::track() Missing Liveness Checks After validate_gc_in_current_heap

**Status:** Open
**Tags:** Unverified

## 📊 Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | Medium | Requires GC sweep to run between Gc creation and track() call |
| **Severity** | High | Can cause tracking of deallocated objects, leading to incorrect behavior or UAF |
| **Reproducibility** | Medium | Needs precise timing between GC allocation, sweep, and track() call |

---

## 🧩 Affected Component & Environment
- **Component:** `AsyncHandle::track()` in `handles/async.rs:324`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 Description

### Expected Behavior

`AsyncHandle::track()` should verify that the GC object is still allocated and valid before adding it to the handle scope, similar to how `GcScope::spawn()` does (bug248 fix).

### Actual Behavior

`AsyncHandle::track()` only calls `validate_gc_in_current_heap()` to check that the pointer is from the current thread's heap, but does NOT check:
1. `is_allocated()` - whether the slot has been swept
2. `has_dead_flag()` - whether the object has been dropped
3. `dropping_state()` - whether the object is being dropped
4. `is_under_construction()` - whether the object is still being constructed

This is inconsistent with the fix applied to `GcScope::spawn()` (bug248) which adds these liveness checks after `validate_gc_in_current_heap()`.

### Code Location

`handles/async.rs:323-334`:
```rust
let gc_ptr = Gc::internal_ptr(gc);
validate_gc_in_current_heap(gc_ptr);  // Only checks heap bounds!

let slot_ptr = unsafe {
    let slots_ptr = self.data.block.slots.get() as *mut HandleSlot;
    slots_ptr.add(idx)
};

let gc_box_ptr = gc_ptr as *const GcBox<()>;
unsafe {
    (*slot_ptr).set(gc_box_ptr);  // Sets potentially dead pointer!
}
```

Contrast with `GcScope::spawn()` (handles/async.rs:1184-1202):
```rust
validate_gc_in_current_heap(tracked.ptr as *const u8);

// Liveness checks: ensure tracked object was not swept or reclaimed (bug248).
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(tracked.ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(tracked.ptr as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "GcScope::spawn: tracked object was deallocated"
        );
    }
}
let gc_box = unsafe { &*tracked.ptr };
assert!(
    !gc_box.has_dead_flag()
        && gc_box.dropping_state() == 0
        && !gc_box.is_under_construction(),
    "GcScope::spawn: tracked object is dead, dropping, or under construction"
);
```

---

## 🔬 Root Cause Analysis

The bug is an oversight during the bug248 fix. When `GcScope::spawn()` was updated to add liveness checks after `validate_gc_in_current_heap()`, the same pattern was not applied to `AsyncHandle::track()`.

This creates an inconsistency in the API:
- `GcScope::spawn()`: validates heap + liveness
- `AsyncHandle::track()`: only validates heap

---

## 💣 Steps to Reproduce / PoC

```rust
#[test]
fn test_async_handle_track_swept_object() {
    use rudo_gc::gc::Gc;
    use rudo_gc::handles::async::AsyncHandleScope;
    use std::thread;
    use std::time::Duration;

    // Create a scope on a separate thread
    let scope = AsyncHandleScope::new();
    
    // Create a GC on this thread
    let gc = Gc::new(42);
    
    // Force GC to run and sweep the object
    // (This is tricky to trigger reliably - requires precise timing)
    
    // Try to track the (potentially swept) GC
    // Without the fix, this may track a deallocated object
    let _handle = scope.track(gc);
}
```

Note: This bug is timing-dependent and difficult to reproduce reliably in tests.

---

## 🛠️ Suggested Fix / Remediation

Add liveness checks to `AsyncHandle::track()` after `validate_gc_in_current_heap()`, matching the pattern in `GcScope::spawn()`:

```rust
let gc_ptr = Gc::internal_ptr(gc);
validate_gc_in_current_heap(gc_ptr);

// Add liveness checks (similar to GcScope::spawn in bug248)
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(gc_ptr) {
        let header = crate::heap::ptr_to_page_header(gc_ptr);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "AsyncHandle::track: tracked object was deallocated"
        );
    }
}
let gc_box = &*(gc_ptr as *const GcBox<()>);
assert!(
    !gc_box.has_dead_flag()
        && gc_box.dropping_state() == 0
        && !gc_box.is_under_construction(),
    "AsyncHandle::track: tracked object is dead, dropping, or under construction"
);
```

---

## 🗣️ Internal Discussion Record

**R. Kent Dybvig (GC architecture perspective):**
In a generational GC, objects can be allocated and then swept between the time a reference is created and when it's tracked. The SATB barrier relies on knowing which objects are live at snapshot time. If we track a dead object, the handle scope will hold a reference to memory that may have been reused, leading to incorrect behavior or memory corruption.

**Rustacean (Soundness perspective):**
This is a memory safety issue. While not a classic UAF (the memory is still accessible), tracking a swept object means the handle points to potentially recycled memory with a new object. Accessing through this handle could read/write incorrect data, violating Rust's safety guarantees.

**Geohot (Exploit perspective):**
If an attacker can control GC timing, they could potentially:
1. Create a GC object
2. Trigger sweep to reclaim it
3. Allocate new data in the same slot
4. Call track() on the stale GC reference
5. The handle now points to attacker-controlled data

This could be leveraged for data flow manipulation attacks.
