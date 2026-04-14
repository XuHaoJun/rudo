# [Bug]: mark_object reads generation from deallocated slot when is_allocated is false

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires lazy sweep to deallocate slot between try_mark and is_allocated re-check |
| **Severity (嚴重程度)** | `High` | UB from reading deallocated slot, stale mark may not be cleared |
| **Reproducibility (復現難度)** | `Medium` | Concurrent lazy sweep needed, stress tests can trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object` (gc/gc.rs:2416-2425)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When `try_mark` succeeds and then `is_allocated` re-check fails (slot was swept), the code should:
1. ALWAYS clear the stale mark
2. Return without reading generation from the deallocated slot

### 實際行為 (Actual Behavior)

In `mark_object` (gc.rs:2416-2425):

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        let current_generation = (*ptr.as_ptr()).generation();  // BUG: UB - reading deallocated slot
        if current_generation != marked_generation {
            return;  // BUG: Returns WITHOUT clearing mark if generations match!
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

**Problems:**
1. When `is_allocated(idx)` is false, the code reads `current_generation` from a potentially deallocated slot - UB
2. If `current_generation == marked_generation` (which shouldn't happen with proper values, but could due to memory reuse), the code returns WITHOUT clearing the stale mark

---

## 🔬 根本原因分析 (Root Cause Analysis)

**The buggy sequence:**

1. `try_mark` succeeds at line 2416, marking the slot
2. First `is_allocated` check at line 2404 passes (slot allocated at that moment)
3. `marked_generation` captured at line 2417
4. Lazy sweep deallocates the slot between lines 2417 and 2418
5. Second `is_allocated` check at line 2418 fails (slot not allocated)
6. **BUG**: Code reads `current_generation` from a deallocated slot (UB!)
7. **BUG**: If `current_generation == marked_generation`, returns WITHOUT clearing mark (stale mark left)

**Correct pattern** (from `mark_and_trace_incremental` gc.rs:2470-2479):
```rust
let marked_generation = (*ptr.as_ptr()).generation();
if !(*header.as_ptr()).is_allocated(idx) {
    // Slot was swept - ALWAYS clear stale mark when slot not allocated.
    // The generation was captured while slot was still valid.
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
if (*ptr.as_ptr()).generation() != marked_generation {
    // Slot was reused with new object - clear stale mark.
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

---

## 💣 重現步驟 / 概念驗證 (PoC)

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

Remove the reading of `current_generation` and use the captured `marked_generation` for the generation check:

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        // FIX bugXXX: Slot was swept - ALWAYS clear stale mark.
        // Do NOT read generation from deallocated slot.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    if (*ptr.as_ptr()).generation() != marked_generation {
        // FIX bugXXX: Slot was reused - clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is for detecting slot REUSE, not for distinguishing swept from swept+reused. When `is_allocated` is false, the slot is dead and the mark must be cleared. The generation check should only run when the slot is guaranteed to be allocated.

**Rustacean (Soundness 觀點):**
Reading `current_generation` from a deallocated slot is undefined behavior. The code should use the captured `marked_generation` for the comparison when `is_allocated` is false, or simply skip the generation check entirely since the slot is deallocated.

**Geohot (Exploit 觀點):**
A stale mark could cause the GC to retain an object that should have been collected. Combined with the UB from reading deallocated memory, this is a serious correctness issue.

---

## 相關 Issue

- bug553: worker_mark_loop same bug pattern (fixed in marker.rs)
- bug549: mark_and_trace_incremental generation mismatch should clear stale mark
- bug547: mark_and_trace_incremental missing is_under_construction check
- bug552: mark_and_trace_incremental returns without clearing mark when slot swept