# [Bug]: GcCell::borrow_mut() uses cached incremental_active for marking NEW pointers, inconsistent with GcThreadSafeCell/GcRwLock

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires incremental or generational barrier to become active during borrow_mut critical section |
| **Severity (嚴重程度)** | High | SATB inconsistency can cause young objects to be prematurely collected |
| **Reproducibility (復現難度)** | Medium | Requires precise timing of barrier state transition during critical section |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut()` (`cell.rs:154-221`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
All three cell types (GcCell, GcThreadSafeCell, GcRwLock) should consistently mark NEW GC pointers when OLD pointers are recorded to maintain SATB invariant.

### 實際行為 (Actual Behavior)
`GcCell::borrow_mut()` uses a cached `incremental_active` value when deciding whether to mark NEW pointers, but `GcThreadSafeCell::borrow_mut()` and `GcRwLock::write()` always mark NEW pointers.

**GcCell::borrow_mut() at cell.rs:204:**
```rust
if generational_active || incremental_active {  // Uses CACHED value
    // mark NEW pointers
}
```

**GcThreadSafeCell::borrow_mut() at cell.rs:1116-1125:**
```rust
// No conditional - unconditionally marks NEW
if !new_gc_ptrs.is_empty() {
    crate::heap::with_heap(|_heap| {
        for gc_ptr in new_gc_ptrs {
            let _ = crate::gc::incremental::mark_object_black(...);
        }
    });
}
```

**GcRwLock::write() at sync.rs:300:**
```rust
mark_gc_ptrs_immediate(&*guard, true);  // Always marks (passes true)
```

### 觸發條件 (Trigger Condition)
If `incremental_active` or `generational_active` transitions from FALSE to TRUE between:
1. When the barrier state is cached (lines 167-168)
2. When NEW pointers are marked (line 204)

Then:
- OLD pointers ARE recorded (line 174 checks `generational_active || incremental_active` at that point)
- NEW pointers are NOT marked (line 204 uses cached value which is false)

This violates SATB invariant: OLD recorded but corresponding NEW not marked.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `GcCell::borrow_mut()`:

**Lines 167-168:** Cache barrier state
```rust
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
let generational_active = crate::gc::incremental::is_generational_barrier_active();
```

**Line 174:** Records OLD pointers using current state (`generational_active || incremental_active`)
```rust
value.capture_gc_ptrs_into(&mut gc_ptrs);
if !gc_ptrs.is_empty() {
    crate::heap::with_heap(|heap| {
        for gc_ptr in gc_ptrs {
            if !heap.record_satb_old_value(gc_ptr) {  // Uses current state
```

**Line 204:** Marks NEW pointers using CACHED state
```rust
if generational_active || incremental_active {  // Uses CACHED value!
    // mark NEW pointers
}
```

The inconsistency:
- OLD recording uses current barrier state (line 174)
- NEW marking uses cached barrier state (line 204)

If barrier becomes active between these two points, OLD is recorded but NEW is not marked.

**Comparison with GcThreadSafeCell::borrow_mut():**
- OLD recording uses current state (like GcCell)
- NEW marking is UNCONDITIONAL (unlike GcCell)

**Comparison with GcRwLock::write():**
- OLD recording uses `true` (unconditional)
- NEW marking uses `true` (unconditional)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, GcCapture};
use std::cell::RefMut;

#[derive(Clone, Trace, GcCapture)]
struct Data {
    gc_field: Gc<i32>,
}

fn main() {
    // Setup: Create old data
    let old_gc = Gc::new(42);
    let cell = GcCell::new(Data { gc_field: old_gc });
    
    // Promote to old generation via full collection
    rudo_gc::collect_full();
    
    // Spawn thread that will trigger incremental marking during borrow_mut
    let handle = std::thread::spawn(move || {
        // Small sleep to let main thread enter critical section
        std::thread::sleep(std::time::Duration::from_micros(100));
        
        // Trigger incremental marking
        rudo_gc::gc::incremental::IncrementalMarkState::global()
            .request_fallback(rudo_gc::gc::incremental::FallbackReason::Test);
    });
    
    // Enter critical section - cache barrier state (both false)
    let mut borrow = cell.borrow_mut();
    
    // At this point, if incremental becomes active:
    // - OLD pointers (old_gc) ARE recorded via record_satb_old_value
    // - But NEW pointers are NOT marked (cached incremental_active is false)
    
    // Modify to point to new young object
    let new_gc = Gc::new(100);
    borrow.gc_field = new_gc.clone();
    
    // Drop borrow - NEW not marked because cached incremental_active was false
    drop(borrow);
    
    handle.join().unwrap();
    
    // Young object (new_gc) may be prematurely collected!
    // Because it was not marked during the barrier
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change `GcCell::borrow_mut()` to always mark NEW pointers when OLD pointers were recorded, similar to `GcThreadSafeCell::borrow_mut()`:

```rust
// Lines 204-219 in cell.rs
// BEFORE (buggy):
if generational_active || incremental_active {
    unsafe {
        let new_value = &*result;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            // ...
        }
    }
}

// AFTER (fixed):
// Always mark NEW pointers when OLD were recorded, to maintain SATB consistency
unsafe {
    let new_value = &*result;
    let mut new_gc_ptrs = Vec::with_capacity(32);
    new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
    if !new_gc_ptrs.is_empty() {
        crate::heap::with_heap(|_heap| {
            for gc_ptr in new_gc_ptrs {
                let _ = crate::gc::incremental::mark_object_black(
                    gc_ptr.as_ptr() as *const u8
                );
            }
        });
    }
}
```

This matches the behavior of:
- `GcThreadSafeCell::borrow_mut()` (lines 1116-1125)
- `GcRwLock::write()` (line 300)

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB invariant requires that if OLD pointers are recorded, corresponding NEW pointers must be marked. When a barrier becomes active during the critical section, GcCell records OLD but not NEW, violating this invariant. This can cause young objects reachable only from the NEW pointer to be prematurely collected.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. If a young object is not marked and is later collected, any access to that object via the Gc pointer would be a use-after-free. The inconsistency between GcCell and the other two cell types creates unpredictable behavior.

**Geohot (Exploit 觀點):**
An attacker could exploit this by timing GC operations to cause inconsistent barrier behavior. The race condition window is small (between barrier state cache and marking), but with precise timing, an attacker could cause memory corruption.

---

## 備註

- Related to bug479 (GcRwLock::write) which was fixed by always marking
- Related to bug486 (GcCell::borrow_mut SATB capture) which fixed OLD recording but not NEW marking
- Inconsistency: GcThreadSafeCell and GcRwLock always mark, GcCell conditionally marks
