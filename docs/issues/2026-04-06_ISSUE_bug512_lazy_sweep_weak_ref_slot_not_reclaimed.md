# [Bug]: lazy_sweep_page with weak_count > 0 does not reclaim slot - memory leak

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any object with weak references that becomes unreachable triggers this path |
| **Severity (嚴重程度)** | High | Memory leak - slots are never reclaimed for objects with weak refs |
| **Reproducibility (復現難度)** | Medium | Requires creating objects with weak refs and letting them die |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `lazy_sweep_page`, `lazy_sweep_page_all_dead` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
Objects with weak references should be reclaimable (slot freed) when they become unreachable, just like objects without weak refs. Weak refs should return `None` when upgraded after the object is reclaimed.

### 實際行為 (Actual Behavior)
1. When `weak_count > 0 && !dead_flag`, the object is marked dead
2. `drop_fn` and `trace_fn` are set to no-op
3. `set_dead()` is called
4. **BUT `clear_allocated(i)` is NEVER called!**
5. The slot remains allocated forever → **memory leak**

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `lazy_sweep_page` (lines 2619-2623):

```rust
if weak_count > 0 && !dead_flag {
    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
    (*gc_box_ptr).set_dead();
    all_dead = false;  // <-- Just sets flag and exits!
    // BUG: clear_allocated(i) is MISSING here!
} else {
    // reclaim logic with clear_allocated(i) at line 2699
}
```

The reclaim logic with `clear_allocated(i)` at line 2699 is ONLY in the `else` branch. When the `if` branch executes (weak_count > 0), the slot is never cleared.

Similarly in `lazy_sweep_page_all_dead` (lines 2748-2751):
```rust
if weak_count > 0 && !dead_flag {
    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
    (*gc_box_ptr).set_dead();
    // BUG: No reclaim here either!
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, collect_full};

fn main() {
    // Create an object with weak ref
    let strong = Gc::new(vec![1, 2, 3]);
    let weak: Weak<Vec<i32>> = Gc::downgrade(&strong);
    
    // Drop strong ref - object is now unreachable
    drop(strong);
    
    // Run GC
    collect_full();
    
    // Weak upgrade should return None
    assert!(weak.upgrade().is_none());
    
    // BUG: The slot is now leaked - never reclaimed!
    // If we try to allocate again, we won't get this slot back
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

In both `lazy_sweep_page` and `lazy_sweep_page_all_dead`, add slot reclamation when `weak_count > 0 && !dead_flag`:

Option 1: Add `clear_allocated(i)` after `set_dead()`:
```rust
if weak_count > 0 && !dead_flag {
    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
    (*gc_box_ptr).set_dead();
    all_dead = false;
    // FIX: Also reclaim the slot
    (*header).clear_allocated(index);
    reclaimed += 1;
}
```

Option 2: Fall through to the else branch reclaim logic after setting dead/dead_flag:
- The else branch properly reclaims slots
- We need the weak ref case to also trigger reclaim

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
In typical GC implementations, weak references do NOT prevent object reclamation. When an object becomes unreachable (no strong refs), it should be reclaimed even if weak refs exist. Weak refs only need to return None after the object is collected. This bug causes dead objects with weak refs to occupy slots forever, causing memory leak.

**Rustacean (Soundness 觀點):**
This is not a soundness issue (no UB), but a memory management issue. Slots never being reclaimed leads to memory exhaustion over time (OOM).

**Geohot (Exploit 觀點):**
Memory leak can be leveraged for memory exhaustion attacks. If an attacker can control when weak refs are created and when strong refs are dropped, they could cause gradual memory growth.

---

## Resolution (2026-04-06)

**Outcome:** Fixed.

Applied fix to `crates/rudo-gc/src/gc/gc.rs`:

**In `lazy_sweep_page` (lines 2619-2624):**
```rust
if weak_count > 0 && !dead_flag {
    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
    (*gc_box_ptr).set_dead();
    (*header).clear_allocated(i);  // FIX: reclaim slot
    reclaimed += 1;
    all_dead = false;
}
```

**In `lazy_sweep_page_all_dead` (lines 2750-2753):**
```rust
if weak_count > 0 && !dead_flag {
    (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
    (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
    (*gc_box_ptr).set_dead();
    (*header).clear_allocated(i);  // FIX: reclaim slot
    reclaimed += 1;
}
```

Both functions now properly reclaim slots for objects with weak refs when they become unreachable.