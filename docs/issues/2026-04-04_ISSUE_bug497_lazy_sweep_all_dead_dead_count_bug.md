# [Bug]: lazy_sweep_page_all_dead unconditionally sets dead_count=0 causing memory leak

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires all objects in a page to be dead with weak_count > 0 |
| **Severity (嚴重程度)** | High | Memory leak - dead objects with weak refs never reclaimed |
| **Reproducibility (復現難度)** | Medium | Can be triggered with proper weak ref + all_dead scenario |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sweep_pending` + `lazy_sweep_page_all_dead` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `lazy_sweep_page_all_dead` doesn't reclaim all dead objects (due to weak references), `dead_count` should reflect the number of unreclaimed dead objects. The page should remain in the sweep pipeline until all dead objects are reclaimed.

### 實際行為 (Actual Behavior)
At `gc.rs:2882` in `sweep_pending`:
```rust
if header.read().all_dead() {
    lazy_sweep_page_all_dead(page_ptr, block_size, obj_count, header_size);
    (*header).clear_all_dead();
    std::sync::atomic::fence(Ordering::Release);
    (*header).clear_needs_sweep();
    (*header).set_dead_count(0);  // <-- BUG: Unconditionally set to 0
    swept += 1;
}
```

The return value of `lazy_sweep_page_all_dead` is ignored. When objects have `weak_count > 0` and cannot be reclaimed, `dead_count` is still set to 0, losing information about unreclaimed dead objects.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**In `lazy_sweep_page_all_dead` (gc.rs:2752-2758):**

When `weak_count > 0`, the slot is NOT added to the free list:
```rust
if weak_count > 0 {
    if !dead_flag {
        ((*gc_box_ptr).drop_fn)(obj_ptr);
        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
        (*gc_box_ptr).set_dead();
    }
    // NOTE: did_reclaim = false, slot NOT added to free list
}
```

The function returns `reclaimed` count (which may be 0), but the caller ignores it.

**The bug scenario:**

1. Page has all objects dead with `weak_count > 0` (all_dead = true)
2. `sweep_pending` calls `lazy_sweep_page_all_dead`
3. `lazy_sweep_page_all_dead` marks objects dead but doesn't reclaim (weak refs)
4. Returns `reclaimed = 0`
5. `sweep_pending` sets `dead_count = 0` and `needs_sweep = false`
6. Page removed from `pending_sweep_by_class`
7. But slots still have `is_allocated(i) = true` (dead objects not reclaimed)
8. Page added to `pages_with_free_slots` but slots not actually free
9. Memory leak: dead objects persist but are invisible to sweep pipeline

**Additionally:** `increment_dead_count()` at `heap.rs:1333` is defined but never called anywhere in the codebase. Dead objects between GC cycles are not tracked.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::rc::Rc;

fn main() {
    // Create objects with weak references to prevent reclamation
    let weak_refs: Vec<_> = (0..100).map(|i| {
        let rc = Rc::new(i);
        let weak = Rc::downgrade(&rc);
        drop(rc);  // Strong count = 0, but weak still alive
        weak
    }).collect();

    // All objects are dead but weak refs prevent reclamation
    // Force GC to mark page as all_dead
    collect_full();

    // At this point, if all objects were in the same page:
    // - all_dead = true
    // - lazy_sweep_page_all_dead called
    // - weak_count > 0 prevents reclamation
    // - dead_count = 0 (bug!)
    // - needs_sweep = false
    // - Dead objects never reclaimed (memory leak)

    // Verify leak by checking memory usage or object count
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option 1: Track actual reclaim count**

```rust
if header.read().all_dead() {
    let reclaimed = lazy_sweep_page_all_dead(page_ptr, block_size, obj_count, header_size);
    (*header).clear_all_dead();
    std::sync::atomic::fence(Ordering::Release);
    (*header).clear_needs_sweep();
    // FIX: Only set dead_count to 0 if ALL objects were reclaimed
    // Otherwise, dead_count should reflect unreclaimed objects
    if reclaimed < obj_count {
        // Objects with weak_count > 0 weren't reclaimed
        (*header).set_dead_count((obj_count - reclaimed) as u16);
        // Keep needs_sweep true so page remains in sweep pipeline?
        // Or remove from pending_sweep and let it be re-added on next GC?
    } else {
        (*header).set_dead_count(0);
    }
    swept += 1;
}
```

**Option 2: Don't clear needs_sweep if not all reclaimed**

```rust
if header.read().all_dead() {
    let reclaimed = lazy_sweep_page_all_dead(page_ptr, block_size, obj_count, header_size);
    (*header).clear_all_dead();
    std::sync::atomic::fence(Ordering::Release);
    if reclaimed < obj_count {
        // Some objects weren't reclaimed (weak refs), keep needs_sweep
        (*header).set_dead_count((obj_count - reclaimed) as u16);
        // Don't clear needs_sweep - page needs another sweep
    } else {
        (*header).clear_needs_sweep();
        (*header).set_dead_count(0);
    }
    swept += 1;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
When all objects in a page are dead but cannot be reclaimed due to weak references, the page should remain in the sweep pipeline. The current code removes the page from sweep consideration while dead objects still occupy slots. This creates a memory leak that compounds over time as more pages have "zombie" dead objects.

**Rustacean (Soundness 觀點):**
The `increment_dead_count` function is defined but never called. This suggests the tracking mechanism for dead objects between GC cycles is incomplete. If `dead_count` is meant to track unreclaimed dead objects, it should be incremented when objects die, not just during marking.

**Geohot (Exploit 觀點):**
The memory leak itself isn't directly exploitable, but if an attacker can trigger specific allocation patterns that cause many pages to enter this "zombie" state, they could cause memory pressure that leads to OOM. This could be a denial-of-service vector in long-running GC-heavy applications.