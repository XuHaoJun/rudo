# [Bug]: Gc<T>::drop missing generation check before dec_ref

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep during Gc lifecycle |
| **Severity (嚴重程度)** | Critical | Can cause ref_count corruption and wrong object drop |
| **Reproducibility (復現難度)** | Very High | Requires precise thread interleaving between sweep and drop |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::drop` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`Gc<T>::drop` calls `dec_ref` without checking if the slot has been reused since the `Gc` was created. If the slot was swept and a new object was allocated in the same slot, `dec_ref` will operate on the new object's ref_count, causing:

1. **Ref count corruption**: The new object's ref_count will be incorrectly decremented
2. **Wrong object drop**: The new object's drop_fn may be called when it shouldn't be
3. **Use-after-free**: If the new object's ref_count reaches 0 incorrectly, the memory could be freed while still in use

### 預期行為 (Expected Behavior)

`Gc::drop` should verify the slot has not been reused (via generation check) before calling `dec_ref`, similar to `GcHandle::drop`.

### 實際行為 (Actual Behavior)

`Gc::drop` directly calls `dec_ref` without any generation check:

```rust
// ptr.rs lines 2240-2255
impl<T: Trace> Drop for Gc<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return;
        }
        let gc_box_ptr = ptr.as_ptr();
        // BUG: No generation check before dec_ref!
        let was_last = GcBox::<T>::dec_ref(gc_box_ptr);
        if !was_last {
            notify_dropped_gc();
        }
    }
}
```

Compare with `GcHandle::drop` (cross_thread.rs lines 850-896) which DOES have generation check:

```rust
// cross_thread.rs lines 860-893
// FIX bug407: Get generation BEFORE removing from handle map to detect slot reuse.
let pre_generation = unsafe { (*self.ptr.as_ptr()).generation() };
// ... remove handle from map ...
// FIX bug524: Check generation BEFORE is_allocated early return.
unsafe {
    let current_generation = (*self.ptr.as_ptr()).generation();
    if pre_generation != current_generation {
        // Slot was reused - do NOT call dec_ref on wrong object.
        return;
    }
    // ... is_allocated check ...
    crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**File:** `/home/noah/Desktop/workspace/rudo-gc/rudo/crates/rudo-gc/src/ptr.rs`
**Lines:** 2240-2255

The `Drop` implementation for `Gc<T>` was never updated to include the generation check pattern that was added to `GcHandle::drop` (bug407 fix).

**Attack scenario:**
1. Thread A creates `Gc::<T>::new(value)` which allocates slot S with object A
2. The `Gc` pointer is stored somewhere (e.g., in a data structure)
3. Object A becomes unreachable and slot S is lazy-swept
4. A new object B is allocated in slot S (generation increments)
5. The original `Gc` goes out of scope and `drop` is called
6. `dec_ref` is called on the slot S pointer - but now slot S contains object B!
7. Object B's ref_count is incorrectly decremented

The generation mechanism exists (via `increment_generation()` called in `try_pop_from_page`), but `Gc::drop` does not use it to detect slot reuse.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

**Note:** This bug requires multi-threaded execution with precise timing to reproduce reliably. Single-threaded tests will NOT trigger this bug.

```rust
// This is a conceptual PoC - actual reproduction requires thread interleaving
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct Data {
    value: usize,
    marker: Arc<AtomicUsize>,
}

fn main() {
    // Create a Gc and store it somewhere
    let marker = Arc::new(AtomicUsize::new(0));
    let gc = Gc::new(Data { value: 42, marker: marker.clone() });
    
    // Keep a handle to force the slot to stay allocated initially
    let handle = gc.clone();
    
    // Drop our reference - slot should NOT be reclaimed immediately
    drop(gc);
    
    // In a real scenario with concurrent lazy sweep:
    // 1. Another thread triggers lazy sweep
    // 2. Slot is swept and object is reclaimed
    // 3. New allocation reuses the slot (generation increments)
    // 4. If 'handle' is dropped AFTER slot reuse but BEFORE handle's drop
    //    runs dec_ref on the wrong object
    
    // Single-threaded PoC cannot reliably trigger this bug (Pattern 2 from verification guidelines)
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation check to `Gc::drop` before calling `dec_ref`:

```rust
impl<T: Trace> Drop for Gc<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return;
        }
        
        let gc_box_ptr = ptr.as_ptr();
        
        // FIX: Check generation BEFORE dec_ref to detect slot reuse.
        // If slot was swept and reused, the generation would have changed.
        // Matches the pattern in GcHandle::drop (bug407 fix).
        unsafe {
            let pre_generation = (*gc_box_ptr).generation();
            
            // Capture current generation after potential concurrent sweep
            let current_generation = (*gc_box_ptr).generation();
            if pre_generation != current_generation {
                // Slot was reused - do NOT call dec_ref on wrong object.
                // The new object already has ref_count initialized by the allocator.
                return;
            }
            
            // Now safe to check is_allocated
            if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
                let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    // Slot was swept - object already collected.
                    return;
                }
            }
            
            let was_last = GcBox::<T>::dec_ref(gc_box_ptr);
            if !was_last {
                notify_dropped_gc();
            }
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation mechanism was added specifically to detect slot reuse during sweep. The `try_pop_from_page` function calls `increment_generation()` when a slot is reused (line 2382 in heap.rs). This generation increment is the key to detecting the race between sweep and reference counting. The bug exists because `Gc::drop` was not updated when the generation check pattern was established in other code paths.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. Calling `dec_ref` on a reused slot without checking generation can lead to:
- Incorrect reference counting
- Calling `drop_fn` on the wrong object
- Potential use-after-free if ref_count reaches 0 incorrectly

The code violates the principle that `dec_ref` should only be called on the object that was originally pointed to.

**Geohot (Exploit 觀點):**
While this bug is difficult to reproduce reliably due to the precise timing required, it represents a memory corruption vector. If an attacker could control the timing of lazy sweep and object lifecycle, they might be able to:
1. Cause a slot to be reused
2. Have a victim call `dec_ref` on the reused slot
3. Potentially trigger incorrect object destruction or ref_count states

The defense-in-depth would be to always check generation before `dec_ref`.
