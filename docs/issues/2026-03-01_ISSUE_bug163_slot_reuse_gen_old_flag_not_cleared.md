# [Bug]: Slot Reuse Does Not Clear GEN_OLD_FLAG

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Every slot reuse after GC collection will retain the stale flag |
| **Severity (嚴重程度)** | Medium | Causes incorrect generational barrier behavior |
| **Reproducibility (復現難度)** | Medium | Can be detected by checking has_gen_old_flag on newly allocated objects in reclaimed slots |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Slot allocation / Sweep phase / `try_pop_from_page`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

When a slot is reclaimed by the GC and added to the free list, and then reused for a new allocation, the `GEN_OLD_FLAG` is NOT cleared. This causes newly allocated objects in reclaimed slots to incorrectly inherit the `GEN_OLD_FLAG` from the previous object.

### 預期行為 (Expected Behavior)
When a slot is reused after GC collection, the `GEN_OLD_FLAG` should be cleared, similar to how `DEAD_FLAG` is cleared. The comment in `ptr.rs:363-364` explicitly states: "Clear GEN_OLD_FLAG. Used when deallocating so reused slots don't inherit stale state."

### 實際行為 (Actual Behavior)
- `try_pop_from_page` (heap.rs:2165-2166) only calls `clear_dead()`, NOT `clear_gen_old()`
- `sweep_phase2_reclaim` (gc.rs) does NOT call `clear_gen_old()` anywhere
- Only `adopt_orphan_pages` (heap.rs:2592) calls `clear_gen_old()`

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Bug Location 1: `try_pop_from_page` (heap.rs:2157-2178)**

When allocating from a free list:
```rust
// Line 2161-2167:
// Clear DEAD_FLAG so reused slot is not incorrectly marked as dead.
// SAFETY: obj_ptr points to a valid GcBox slot (was in free list).
#[allow(clippy::cast_ptr_alignment)]
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();  // <-- Only clears DEAD_FLAG
    // MISSING: (*gc_box_ptr).clear_gen_old();
}
```

**Bug Location 2: `sweep_phase2_reclaim` (gc.rs:2260-2281)**

When reclaiming slots:
```rust
if is_alloc && !is_marked {
    // Slot is allocated but not marked - candidate for reclamation
    // ... 
    (*header).clear_allocated(i);
    // MISSING: (*gc_box_ptr).clear_gen_old();
}
```

**Root Cause:**
1. During major GC promotion (gc.rs:1698-1710), ALL surviving objects have `set_gen_old()` called
2. When those objects are eventually collected, their slots are added to the free list without clearing `GEN_OLD_FLAG`
3. New objects allocated in those slots incorrectly have `GEN_OLD_FLAG` set

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

**Fix 1: In `try_pop_from_page` (heap.rs:2165-2167)**

Add `clear_gen_old()` call after `clear_dead()`:
```rust
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();
    (*gc_box_ptr).clear_gen_old();  // ADD THIS LINE
}
```

**Fix 2: In `sweep_phase2_reclaim` (gc.rs:2260-2281)**

Add `clear_gen_old()` call when reclaiming slots:
```rust
if is_alloc && !is_marked {
    // ...
    (*gc_box_ptr).clear_gen_old();  // ADD THIS LINE
    (*header).clear_allocated(i);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The `GEN_OLD_FLAG` is an optimization that allows the write barrier to skip early-exit checks when the parent object is young. When this flag incorrectly persists on new objects, the barrier may:
- Not fire when it should (if the check is based on this flag)
- Cause incorrect OLD→YOUNG reference tracking during minor GC

**Rustacean (Soundness 觀點):**
This is not a soundness bug per se - the objects are still valid. However, it's a correctness bug in the GC that could lead to:
- Memory leaks (if barrier doesn't fire and references are lost)
- Premature collection (unlikely given the flag's purpose)

**Geohot (Exploit 觀點):**
The stale flag could potentially be exploited if there's code that behaves differently based on `has_gen_old_flag()`. Combined with other bugs, this could lead to:
- Confusion in barrier behavior
- Potential for confused-deputy style attacks if any security-critical code relies on generational separation
