# [Bug]: Slot Allocation in try_pop_from_page Does Not Clear GEN_OLD_FLAG

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Every slot reuse during allocation will retain the stale flag |
| **Severity (嚴重程度)** | Medium | Causes incorrect generational barrier behavior |
| **Reproducibility (復現難度)** | Medium | Can be detected by checking has_gen_old_flag on newly allocated objects in reclaimed slots |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Slot allocation / `try_pop_from_page` / heap.rs:2206-2212
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

When a slot is reclaimed by the GC and then reused for a new allocation via `try_pop_from_page`, the `GEN_OLD_FLAG` is NOT cleared. This causes newly allocated objects in reclaimed slots to incorrectly inherit the `GEN_OLD_FLAG` from the previous object.

This is related to bug163 but focuses specifically on the allocation path (try_pop_from_page), whereas bug163 covers multiple paths including the sweep phase.

### 預期行為 (Expected Behavior)
When a slot is reused after GC collection in the allocation path, both `DEAD_FLAG` AND `GEN_OLD_FLAG` should be cleared to prevent stale state from affecting new allocations.

### 實際行為 (Actual Behavior)
- `try_pop_from_page` (heap.rs:2211) only calls `clear_dead()`, NOT `clear_gen_old()`
- The dealloc path (heap.rs:2637) calls `clear_gen_old()` but not in allocation path

### 程式碼位置
- **有問題的程式碼**: `heap.rs:2206-2212`

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `try_pop_from_page` (heap.rs:2206-2212):

```rust
// Clear DEAD_FLAG so reused slot is not incorrectly marked as dead.
// SAFETY: obj_ptr points to a valid GcBox slot (was in free list).
#[allow(clippy::cast_ptr_alignment)]
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();  // <-- Only clears DEAD_FLAG
    // MISSING: (*gc_box_ptr).clear_gen_old();
}
```

The issue:
1. During major GC promotion (gc.rs:1698-1710), ALL surviving objects have `set_gen_old()` called
2. When those objects are eventually collected, their slots are added to the free list without clearing `GEN_OLD_FLAG`
3. When slots are reused via `try_pop_from_page`, only `clear_dead()` is called, NOT `clear_gen_old()`
4. New objects allocated in those slots incorrectly have `GEN_OLD_FLAG` set

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This is a conceptual PoC - needs verification
use rudo_gc::{Gc, GcCell, Trace, collect_full};

#[derive(Trace)]
struct TestObj {
    value: GcCell<i32>,
}

fn main() {
    // Allocate and promote an object to old generation
    let old_obj = Gc::new(TestObj {
        value: GcCell::new(1),
    });
    
    // Major GC to promote to old gen (sets GEN_OLD_FLAG)
    collect_full();
    
    // Drop the object - slot becomes free
    drop(old_obj);
    
    // Force GC to reclaim the slot
    collect_full();
    
    // Allocate new object in what was the same slot
    let new_obj = Gc::new(TestObj {
        value: GcCell::new(2),
    });
    
    // Check if GEN_OLD_FLAG is incorrectly set on new_obj
    // This requires accessing internal APIs or checking barrier behavior
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

In `try_pop_from_page` (heap.rs:2211), add `clear_gen_old()` call after `clear_dead()`:

```rust
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();
    (*gc_box_ptr).clear_gen_old();  // ADD THIS LINE
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- The `GEN_OLD_FLAG` is an optimization that allows the write barrier to skip early-exit checks when the parent object is young
- When this flag incorrectly persists on new objects, the barrier may not fire when it should, causing incorrect OLD→YOUNG reference tracking during minor GC

**Rustacean (Soundness 觀點):**
- This is not a soundness bug - objects are still valid
- However, it's a correctness bug in the GC that could lead to memory leaks or premature collection

**Geohot (Exploit 觀點):**
- The stale flag could potentially be exploited if there's code that behaves differently based on `has_gen_old_flag()`

---

## Resolution (2026-03-03)

**Outcome:** Fixed.

Added `(*gc_box_ptr).clear_gen_old()` in `try_pop_from_page` (heap.rs) immediately after `clear_dead()`, matching the dealloc path (heap.rs:2640) and sweep paths (gc.rs:2536, 2660). Ensures new objects in reclaimed slots never inherit stale `GEN_OLD_FLAG` from the previous occupant.
