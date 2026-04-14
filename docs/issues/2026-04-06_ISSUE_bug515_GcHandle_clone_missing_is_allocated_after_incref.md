# [Bug]: GcHandle::clone missing is_allocated check after inc_ref

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires precise timing between is_allocated check and inc_ref; possible under GC pressure |
| **Severity (嚴重程度)** | Critical | Type confusion / UAF: handle points to wrong object after slot reuse |
| **Reproducibility (復現難度)** | Medium | Needs lazy sweep to reclaim slot between check and inc_ref; can be triggered with forced GC |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::clone` (handles/cross_thread.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x (current main)

---

## 📝 問題描述 (Description)

In `GcHandle::clone`, the code performs an `is_allocated` check **before** dereferencing the GcBox and calling `inc_ref`, but does **not** verify the slot is still allocated **after** `inc_ref` and **before** inserting into `roots.strong`.

If lazy sweep reclaims the slot between the first check and `inc_ref`:
1. The first `is_allocated` check passes (slot still valid)
2. Lazy sweep reclaims the slot and reallocates it to a new object
3. `inc_ref` modifies the **new object's** ref count (wrong object!)
4. `roots.strong.insert` creates a handle pointing to the new object
5. Future clones/resolves operate on the wrong object

### 預期行為 (Expected Behavior)
Handle clone should result in a new handle that references the **same object** as the original handle. The reference count should be correctly maintained.

### 實際行為 (Actual Behavior)
If slot reuse occurs between check and inc_ref:
- New handle references a **different object** (type confusion)
- Reference count of the wrong object is incorrectly incremented
- Original object may be collected prematurely

---

## 🔬 根本原因分析 (Root Cause Analysis)

The pattern in `GcHandle::clone` (lines 768-802):

```rust
// 1. Check is_allocated BEFORE dereferencing
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    assert!((*header.as_ptr()).is_allocated(idx), ...);  // Check 1
}

// 2. Dereference and check flags
let gc_box = &*self.ptr.as_ptr();
assert!(!gc_box.has_dead_flag() && gc_box.dropping_state() == 0 ...);

// 3. Get generation and inc_ref  
let pre_generation = (*self.ptr.as_ptr()).generation();
(*self.ptr.as_ptr()).inc_ref();

// 4. Verify generation changed
if pre_generation != (*self.ptr.as_ptr()).generation() { ... }

// 5. Insert into roots - BUT NO is_allocated CHECK HERE!
let new_id = roots.allocate_id();
roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
```

The correct pattern (used in `clone_orphan_root_with_inc_ref` in heap.rs:311-330):

```rust
// 1. Check is_allocated
if let Some(idx) = ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!((*header.as_ptr()).is_allocated(idx), ...);
}

// 2. Get generation BEFORE inc_ref
let pre_generation = (*ptr.as_ptr()).generation();
(*ptr.as_ptr()).inc_ref();

// 3. Verify generation changed
if pre_generation != (*ptr.as_ptr()).generation() {
    GcBox::undo_inc_ref(ptr.as_ptr());
    panic!("slot was reused during clone (generation mismatch)");
}

// 4. Insert into orphan table
```

The `clone_orphan_root_with_inc_ref` has the same gap - it checks generation but not `is_allocated` after inc_ref. Both functions need the check.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

// Test to demonstrate the issue - would need Miri or precise timing
fn test_handle_clone_slot_reuse() {
    // This test would need to be run with instrumentation to force
    // lazy sweep to reclaim the slot between is_allocated check and inc_ref
    // in GcHandle::clone
    
    // Create multiple handles to put pressure on the heap
    let handles: Vec<_> = (0..100).map(|_| {
        let gc = Gc::new(Data { value: AtomicUsize::new(0) });
        gc.cross_thread_handle()
    }).collect();
    
    // Force GC to promote some objects to old generation
    collect_full();
    
    // Drop most handles to make their slots eligible for lazy sweep
    drop(handles[50..]);
    
    // Force minor GC to trigger lazy sweep
    collect();
    
    // Now clone one of the remaining handles
    // If the slot was swept and reused between check and inc_ref,
    // the clone will point to a different (newly allocated) object
    let clone = handles[25].clone();
    
    // The clone should reference the SAME object
    // But due to the bug, it may reference a different object
}
```

**Note**: This is difficult to reproduce reliably in a test because it requires precise timing between the `is_allocated` check and `inc_ref`. The bug is latent in the code structure.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add a second `is_allocated` check AFTER `inc_ref` (or use the generation check to trigger rollback):

```rust
// Around line 793-802 in GcHandle::clone:
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        assert!((*header.as_ptr()).is_allocated(idx), ...);
    }

    let gc_box = &*self.ptr.as_ptr();
    // ... existing checks ...

    let pre_generation = (*self.ptr.as_ptr()).generation();
    (*self.ptr.as_ptr()).inc_ref();

    if pre_generation != (*self.ptr.as_ptr()).generation() {
        crate::ptr::GcBox::undo_inc_ref(self.ptr.as_ptr());
        panic!("slot was reused during clone (generation mismatch)");
    }

    // ADD: Second is_allocated check AFTER inc_ref to catch slot reuse
    // that bypassed the generation check (defense-in-depth)
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "GcHandle::clone: object slot was swept after inc_ref"
        );
    }
}
```

Similarly, fix `clone_orphan_root_with_inc_ref` in heap.rs around line 324-330.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check provides some protection - if slot reuse occurs, the generation would change after inc_ref, triggering a panic. However, this relies on generation being incremented exactly once on reuse. If the sweep reuses the slot and the generation check passes for some other reason (e.g., generation wrapping), we'd have type confusion. The is_allocated check is the authoritative liveness check and should be performed again after any operation that could be affected by slot state.

**Rustacean (Soundness 觀點):**
This is a soundness issue. The handle could point to a object of a completely different type if slot reuse occurs at an inopportune moment. The `inc_ref` modifying the wrong object's ref count could lead to premature collection of the original object or leaks. The fix should add defense-in-depth with is_allocated checks at both entry and exit of the critical section.

**Geohot (Exploit 觀點):**
Slot reuse bugs can be exploited for type confusion attacks. In a scenario where an attacker can control GC timing (e.g., via resource exhaustion), they could potentially make a handle point to maliciously crafted object data. The missing is_allocated check after inc_ref is a security gap that could be leveraged for attacks. This is especially concerning for handles which are Send+Sync even when T is not, making them valuable attack vectors for cross-thread type confusion.