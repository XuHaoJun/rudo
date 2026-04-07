# [Bug]: GcHandle::unregister() skips generation check when slot not allocated, allowing slot reuse to corrupt ref_count

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires slot to be swept between is_allocated check and generation check |
| **Severity (嚴重程度)** | Critical | Ref count corruption can cause premature drop or memory leaks |
| **Reproducibility (復現難度)** | Medium | Requires precise timing between unregister and sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::unregister()` in `handles/cross_thread.rs:117-154`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcHandle::unregister()` should verify the slot hasn't been reused (generation check) before calling `dec_ref()`. If the slot was swept and reused, it should panic to prevent corrupting another object's ref_count.

### 實際行為 (Actual Behavior)
When `is_allocated()` returns `false` (slot was already swept), `unregister()` returns early without performing the generation check. This allows a TOCTOU race where:

1. Slot is allocated, `GcHandle` holds reference
2. Slot is swept and reused by a new object (new generation)
3. `unregister()` is called
4. `is_allocated()` returns `false` for the new object
5. `unregister()` returns early, skipping generation check and `dec_ref()`
6. New object's ref_count is NOT decremented, causing a leak

The code structure:
```rust
// cross_thread.rs:139-153
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return;  // <-- EARLY RETURN without generation check!
        }
    }
    let current_generation = (*self.ptr.as_ptr()).generation();
    if pre_generation != current_generation {
        panic!("GcHandle::unregister: slot was reused during unregister (generation mismatch)");
    }
}
crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `GcHandle::unregister()` at lines 139-152:

```rust
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return;  // BUG: Early return skips generation check!
        }
    }
    // Generation check is only reached if is_allocated returned true
    let current_generation = (*self.ptr.as_ptr()).generation();
    if pre_generation != current_generation {
        panic!("GcHandle::unregister: slot was reused during unregister (generation mismatch)");
    }
}
```

**Root Cause**: When `is_allocated()` returns `false`, the function returns early without verifying that the generation hasn't changed. If the slot was swept and reused, `dec_ref()` will be called on the new object (with the old handle's `handle_id`), corrupting the new object's ref_count.

**Comparison with other methods**:
- `GcHandle::resolve_impl()` (line 248-254): Performs `is_allocated` check, but continues to verify flags and generation
- `GcHandle::try_resolve_impl()` (line 397-406): Same pattern - performs checks in order
- `Handle::get()` (line 310-351): Has generation check AFTER the is_allocated checks
- `AsyncHandle::to_gc()` (line 851-878): Has generation check after is_allocated checks

The issue is that `unregister()` has an early return when `is_allocated` is false, but doesn't perform the generation check first. The generation check is the definitive way to detect slot reuse.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full, GcHandle};
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_gchandle_unregister_slot_reuse_toctou() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // Drop original Gc, forcing slot to be reclaimable
    drop(gc);
    
    // Force allocation to reclaim the slot (promote young objects, etc.)
    // In real scenario, this happens via GC cycle
    for i in 0..1000 {
        let _temp = Gc::new(Data { value: i });
    }
    
    // The slot is now likely reused by a new object
    // Force full collection to ensure slot reuse
    collect_full();
    
    // Now call unregister - if slot was reused, the generation check
    // should catch it, but the bug causes early return without check
    handle.unregister();
    
    // After unregister, the new object's ref_count may be corrupted
    // because dec_ref was called on the wrong (new) object
}
```

Note: This is a TOCTOU race. In single-threaded tests, the slot reuse may not happen deterministically. Multi-threaded execution or precise GC timing is needed for reliable reproduction.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the generation check BEFORE the `is_allocated` early return, or remove the early return entirely:

```rust
pub fn unregister(&mut self) {
    if self.handle_id == HandleId::INVALID {
        return;
    }

    let pre_generation = unsafe { (*self.ptr.as_ptr()).generation() };

    if let Some(tcb) = self.origin_tcb.upgrade() {
        let mut roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
    } else {
        let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
    }
    self.handle_id = HandleId::INVALID;

    unsafe {
        // Check generation BEFORE is_allocated to detect slot reuse
        // If slot was swept and reused, generation will have changed
        let current_generation = (*self.ptr.as_ptr()).generation();
        if pre_generation != current_generation {
            // Slot was reused - don't call dec_ref on wrong object
            return;
        }
        
        // Now safe to check is_allocated - slot still valid if we reach here
        if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                // Slot was swept after generation check but before dec_ref
                // This means the object was already collected, no need to dec_ref
                return;
            }
        }
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
    }
}
```

**Key changes**:
1. Move generation check BEFORE the `is_allocated` check
2. Use the generation check as the primary slot-reuse detection
3. Only call `dec_ref()` if generation hasn't changed (slot still valid)

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is the authoritative way to detect slot reuse in this system. Every other method that accesses a GcBox after potential sweep uses it as the primary validation. The early return in `unregister()` bypasses this safeguard, which is inconsistent with the rest of the codebase.

**Rustacean (Soundness 觀點):**
This is a TOCTOU race condition. When `is_allocated` returns false, we can't assume the slot is "safe" - we need to verify the generation hasn't changed to ensure we're not operating on a reused slot. The current code has the check but it's placed after the early return, making it unreachable in the swept case.

**Geohot (Exploit 觀點):**
An attacker could potentially exploit this by:
1. Creating a handle to an object
2. Forcing the slot to be swept and reused with a malicious object
3. Calling unregister on the handle
4. The dec_ref would affect the attacker's object, potentially causing use-after-free or memory corruption

---

## 備註

- Related to bug407 which addresses the same pattern in `GcHandle::drop()`
- The fix should follow the same pattern used in `resolve_impl()` and other methods
- Defense-in-depth: Multiple checks (generation, is_allocated) should be used together
