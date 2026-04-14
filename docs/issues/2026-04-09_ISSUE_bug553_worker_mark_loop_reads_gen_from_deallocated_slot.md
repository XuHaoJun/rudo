# [Bug]: worker_mark_loop reads generation from potentially deallocated slot when is_allocated is false

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires lazy sweep to deallocate slot between try_mark and is_allocated check |
| **Severity (嚴重程度)** | `High` | UB from reading deallocated slot, stale mark may not be cleared |
| **Reproducibility (復現難度)** | `Medium` | Concurrent lazy sweep needed, stress tests can trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `worker_mark_loop` (gc/marker.rs:918-924 and 1086-1092)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When `try_mark` succeeds and then `is_allocated` check fails (slot was swept), the code should:
1. ALWAYS clear the stale mark
2. Return without reading generation from the deallocated slot

### 實際行為 (Actual Behavior)

In `worker_mark_loop` (marker.rs:918-924):

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    let gc_box_ptr = obj.cast_mut();
    // FIX bug427: Capture generation to detect slot reuse.
    let marked_generation = (*gc_box_ptr).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        let current_generation = (*gc_box_ptr).generation();  // BUG: UB - reading deallocated slot
        if current_generation != marked_generation {
            break; // Slot was reused - skip
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    // ...
}
```

**Problems:**
1. The inner `if !(*header.as_ptr()).is_allocated(idx)` block at line 918-924 is dead code after the outer check at line 909 already handled the `is_allocated == false` case
2. When the inner block executes (which can happen with different control flow), it reads `current_generation` from a slot that may have been deallocated and reused - UB
3. The generation check logic is inconsistent with `mark_and_trace_incremental` which correctly handles this case

---

## 🔬 根本原因分析 (Root Cause Analysis)

**The buggy sequence:**

1. `try_mark` succeeds, marking the slot
2. First `is_allocated` check passes (slot is allocated at that moment)
3. `marked_generation` captured
4. Lazy sweep deallocates the slot between the two `is_allocated` checks
5. Second `is_allocated` check fails (slot not allocated)
6. **BUG**: Code reads `current_generation` from a deallocated slot (UB!)
7. **BUG**: If `current_generation != marked_generation`, returns WITHOUT clearing mark (stale mark left)

**Correct pattern** (from `mark_and_trace_incremental` gc.rs:2470-2479):
```rust
let marked_generation = (*ptr.as_ptr()).generation();
if !(*header.as_ptr()).is_allocated(idx) {
    // Slot was swept - ALWAYS clear stale mark
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
if (*ptr.as_ptr()).generation() != marked_generation {
    // Slot was reused - clear stale mark
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires concurrent lazy sweep:
// 1. Allocate object A in slot with generation G
// 2. try_mark succeeds on object A
// 3. First is_allocated check passes (slot allocated)
// 4. marked_generation captured = G
// 5. Lazy sweep deallocates slot (object A collected, slot empty)
// 6. Second is_allocated check fails (slot not allocated)
// 7. BUG: reads current_generation from deallocated slot
// 8. BUG: if generations happen to match, returns WITHOUT clearing mark
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Remove the inner `is_allocated` re-check block since the outer check already handles this case:

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    let gc_box_ptr = obj.cast_mut();
    let marked_generation = (*gc_box_ptr).generation();
    // FIX bug553: Remove inner is_allocated re-check block.
    // The outer check at line 909 already handles is_allocated == false.
    // The inner block was dead code that could cause UB by reading generation
    // from a deallocated slot.
    if (*gc_box_ptr).generation() != marked_generation {
        break; // Slot was reused - skip
    }
    // FIX bug469: Skip objects under construction (e.g. Gc::new_cyclic).
    if (*gc_box_ptr).is_under_construction() {
        break;
    }
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    marked += 1;
    ((*gc_box_ptr).trace_fn)(ptr_addr, &mut visitor);
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is for detecting slot REUSE, not for distinguishing swept from swept+reused. When `is_allocated` is false, the slot is dead and the mark must be cleared. The generation check should only run when the slot is guaranteed to be allocated.

**Rustacean (Soundness 觀點):**
Reading `current_generation` from a deallocated slot is undefined behavior. The code should capture generation immediately after `try_mark` succeeds (when slot is guaranteed allocated), then check `is_allocated`, then make decisions based on generation comparison.

**Geohot (Exploit 觀點):**
A stale mark could cause the GC to retain an object that should have been collected. Combined with the UB from reading deallocated memory, this is a serious correctness issue.

---

## 相關 Issue

- bug427: worker_mark_loop missing generation check
- bug469: worker_mark_loop missing is_under_construction check
- bug529: worker_mark_loop missing second is_allocated check before trace_fn
- bug550: mark_and_trace_incremental same bug (fixed)
- bug552: mark_and_trace_incremental reads gen from deallocated slot (fixed)