# [Bug]: GcRwLockWriteGuard and GcMutexGuard drop missing mark_page_dirty_for_borrow call

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any use of GcRwLock or GcMutex with GcCell<Vec<Gc<T>>> children |
| **Severity (嚴重程度)** | High | Objects incorrectly swept during minor GC, causing use-after-free |
| **Reproducibility (復現難度)** | Medium | Can be reproduced with proper multi-generational allocation test |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop()`, `GcMutexGuard::drop()`, write barriers
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When dropping a `GcRwLockWriteGuard` or `GcMutexGuard`, the page containing the lock should be marked dirty to ensure children in `GcCell<Vec<Gc<T>>>` containers are traced during minor GC. This is necessary for consistency with `GcCell::borrow_mut()` and `GcThreadSafeCell::borrow_mut()` which both call `mark_page_dirty_for_borrow()`.

### 實際行為 (Actual Behavior)

`GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()` call `mark_object_black()` and `unified_write_barrier()` but do NOT call `mark_page_dirty_for_borrow()`. This creates an inconsistency:

- `GcCell::borrow_mut()` (cell.rs:204-206): Calls `mark_page_dirty_for_borrow(ptr)`
- `GcThreadSafeCell::borrow_mut()` (cell.rs:1127-1129): Calls `mark_page_dirty_for_borrow(ptr)`
- `GcRwLockWriteGuard::drop()` (sync.rs:468-494): Does NOT call `mark_page_dirty_for_borrow()`
- `GcMutexGuard::drop()` (sync.rs:745-770): Does NOT call `mark_page_dirty_for_borrow()`

Without `mark_page_dirty_for_borrow()`, pages containing `GcRwLock` or `GcMutex` are not marked dirty, so children in `GcCell<Vec<Gc<T>>>` containers may not be traced during minor GC.

### 程式碼位置

**sync.rs:468-494 (GcRwLockWriteGuard::drop):**
```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        // FIX bug409: Re-check current barrier state...
        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();

        if generational_active || incremental_active {
            for gc_ptr in &ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }

        if generational_active || incremental_active {
            let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, incremental_active);
        }
        // BUG: Missing mark_page_dirty_for_borrow() call!
    }
}
```

**sync.rs:745-770 (GcMutexGuard::drop):**
Same issue - missing `mark_page_dirty_for_borrow()` call.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `mark_page_dirty_for_borrow()` function ensures that regardless of the `gen_old` optimization (bug71) state, the page is added to `dirty_pages`. This is critical for minor GC tracing of `GcCell<Vec<Gc<T>>>` children.

The inconsistency arose because:
1. `GcCell::borrow_mut()` and `GcThreadSafeCell::borrow_mut()` were fixed to include `mark_page_dirty_for_borrow()` (bug583, bug610)
2. `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()` were not updated with the same fix

When a lock guard is dropped, if the page is not marked dirty, minor GC won't trace through `GcCell<Vec<Gc<T>>>` children stored within the locked data, potentially causing slot reuse issues similar to bug612.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcRwLock, GcCell, Trace, collect_full};
use std::cell::RefCell;

#[derive(Trace)]
struct Node {
    children: GcCell<Vec<Gc<Self>>>,
}

fn main() {
    // Create root with GcRwLock containing GcCell<Vec<Gc<T>>>
    let root = Gc::new(GcRwLock::new(Node {
        children: GcCell::new(Vec::new()),
    }));
    register_test_root(root.as_ptr());
    
    // Build first tree
    {
        let mut guard = root.write();
        guard.children.borrow_mut().push(Gc::new(Node {
            children: GcCell::new(Vec::new()),
        }));
    }
    
    // Force GC and promote to old gen
    collect_full();
    
    // Build second tree - this may reuse slots from first tree
    {
        let mut guard = root.write();
        guard.children.borrow_mut().push(Gc::new(Node {
            children: GcCell::new(Vec::new()),
        }));
    }
    
    // Access first tree's nodes via root
    // If bug triggers: "Gc::deref: slot has been swept and reused"
    let _first = root.read().children.borrow()[0].clone();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `mark_page_dirty_for_borrow()` call to both `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()`:

**GcRwLockWriteGuard::drop() (sync.rs:491-492):**
```rust
if generational_active || incremental_active {
    let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
    crate::heap::unified_write_barrier(ptr, incremental_active);
    // ADD: Mark page dirty to ensure children are traced during minor GC
    unsafe {
        crate::heap::mark_page_dirty_for_borrow(ptr);
    }
}
```

**GcMutexGuard::drop() (sync.rs:766-768):**
```rust
if generational_active || incremental_active {
    let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
    crate::heap::unified_write_barrier(ptr, incremental_active);
    // ADD: Mark page dirty to ensure children are traced during minor GC
    unsafe {
        crate::heap::mark_page_dirty_for_borrow(ptr);
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The `gen_old` optimization (bug71) is a performance improvement that skips recording OLD→YOUNG references when the page is young and the gen_old flag is not set. However, for minor GC tracing to work correctly with containers like `GcCell<Vec<Gc<T>>>`, the page must be in the `dirty_pages` set. The `mark_page_dirty_for_borrow()` call ensures this regardless of the gen_old optimization state. This fix aligns `GcRwLock` and `GcMutex` with `GcCell` and `GcThreadSafeCell` behavior.

**Rustacean (Soundness 觀點):**
This is a memory safety issue - children in `GcCell<Vec<Gc<T>>>` containers may be incorrectly swept during minor GC, causing use-after-free when accessing slots that have been reused. The slot reuse detection in `Gc::deref` provides a safety net, but this bug can still cause correctness issues.

**Geohot (Exploit 觀點):**
An attacker could trigger this bug by repeatedly allocating and collecting, causing unpredictable object lifetimes. The window for exploitation is during the drop of lock guards where `mark_page_dirty_for_borrow()` is not called - children GC pointers may be stale in the dirty_pages set.