# [Bug]: mark_object_minor same bug as bug550 - reads gen from deallocated slot and returns without clearing mark when slot swept

**Status:** Fixed
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires lazy sweep to deallocate slot between try_mark and generation check |
| **Severity (嚴重程度)** | High | Stale mark left on swept slot, causing incorrect object retention |
| **Reproducibility (復現難度)** | Medium | Concurrent lazy sweep needed, but stress tests can trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_minor` (gc/gc.rs:2103-2110)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When a slot is swept (deallocated) between `try_mark` succeeding and the generation check, the code should ALWAYS clear the mark before returning. The mark is stale and belongs to the dead object.

### 實際行為 (Actual Behavior)

In `mark_object_minor` (gc.rs:2103-2110):

```rust
if !(*header.as_ptr()).is_allocated(index) {
    let current_generation = (*ptr.as_ptr()).generation();  // UB: reading deallocated slot
    if current_generation != marked_generation {
        return;  // BUG: Returns WITHOUT clearing mark when generations match!
    }
    (*header.as_ptr()).clear_mark_atomic(index);
    return;
}
```

When `is_allocated(index)` is false AND `current_generation == marked_generation`, the code returns WITHOUT clearing the mark. This is wrong because:
1. The slot was swept (object is gone) - the mark is stale
2. Even if generations match (slot was swept but not yet reused), we should clear the mark
3. Reading `current_generation` from a deallocated slot is undefined behavior

**This is the exact same bug as bug550** (which was for `mark_and_trace_incremental`), but here in `mark_object_minor`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**The buggy sequence:**

1. `try_mark` succeeds at line 2094, marking the slot
2. `marked_generation` captured at line 2102
3. First `is_allocated` check passes (slot is allocated)
4. Lazy sweep deallocates the slot between lines 2102 and 2103
5. Second `is_allocated` check at line 2103 fails (slot not allocated)
6. **BUG**: Code reads `current_generation` from deallocated slot (UB!)
7. **BUG**: If `current_generation == marked_generation`, returns WITHOUT clearing mark

**Why this is wrong:**

When `is_allocated` is false, the object is gone and the mark is stale. We should ALWAYS clear the mark in this case, regardless of generation. The generation check at line 2105 is meant for when the slot is STILL allocated (slot was reused with new object), not when it was swept.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires concurrent lazy sweep:
// 1. Allocate object A in slot with generation G
// 2. try_mark succeeds on object A
// 3. marked_generation captured = G
// 4. Lazy sweep deallocates slot (object A collected, slot empty)
// 5. Second is_allocated check fails (slot not allocated)
// 6. If current_generation == G (edge case - slot swept but not reused), return WITHOUT clearing mark
// 7. Slot now has stale mark from object A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Remove the buggy inner block (lines 2103-2110) and restructure to correctly handle sweep:

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(index) {
        // FIX BUGXXX (same as bug550): Slot was swept - ALWAYS clear stale mark when slot not allocated
        (*header.as_ptr()).clear_mark_atomic(index);
        return;
    }
    if (*ptr.as_ptr()).generation() != marked_generation {
        // FIX bug549: Slot was reused with new object - clear stale mark
        (*header.as_ptr()).clear_mark_atomic(index);
        return;
    }
    // FIX bug546: Skip objects under construction (e.g. Gc::new_cyclic).
    // Matches worker_mark_loop (bug469), mark_object_black (bug238).
    if (*ptr.as_ptr()).is_under_construction() {
        (*header.as_ptr()).clear_mark_atomic(index);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The mark bit should always be associated with the object that set it. When a slot is swept (object gone), the mark must be cleared regardless of generation. The generation check is for detecting slot REUSE, not for detecting slot SWEEP.

**Rustacean (Soundness 觀點):**
Reading `current_generation` from a deallocated slot is undefined behavior. The code should read generation when slot is guaranteed to be allocated (before `is_allocated` check), then check `is_allocated`, then make decisions.

**Geohot (Exploit 觀點):**
A stale mark could cause the GC to retain an object that should have been collected, leading to memory pressure. Combined with other bugs, this could contribute to memory exhaustion.

---

## 相關 Issue

- bug550: `mark_and_trace_incremental` same bug (returns without clearing mark when slot swept and generations match)
- bug549: generation mismatch should clear stale mark
- bug547: `mark_and_trace_incremental` missing is_under_construction check
- bug546: `mark_object_minor` missing is_under_construction check (FIXED)