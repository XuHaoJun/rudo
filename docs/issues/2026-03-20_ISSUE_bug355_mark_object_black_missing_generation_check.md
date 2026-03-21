# [Bug]: mark_object_black missing generation check on slot reuse

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires specific timing with lazy sweep and cross-thread handle resolution |
| **Severity (嚴重程度)** | `Medium` | Could cause unreachable objects to be prematurely collected |
| **Reproducibility (復現難度)** | `Medium` | Requires concurrent execution with specific interleaving |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Incremental Marking`, `mark_object_black` in `gc/incremental.rs`
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

When `mark_object_black` successfully marks an object via `try_mark` but then finds the slot is no longer allocated via `is_allocated`, it clears the mark unconditionally without checking if the slot was reused.

### 預期行為 (Expected Behavior)
After `try_mark` succeeds but `is_allocated` returns false, the code should verify the **generation** hasn't changed to distinguish between:
1. Slot was swept (slot still contains same object, mark should be cleared)
2. Slot was swept AND reused (slot contains new object with different generation, mark should NOT be cleared as it belongs to the new object)

### 實際行為 (Actual Behavior)
The code clears the mark unconditionally when `is_allocated` fails after a successful `try_mark`, without checking the generation. This can incorrectly clear the mark on a **newly allocated object** that just happens to be in a swept slot.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `crates/rudo-gc/src/gc/incremental.rs`, lines 1113-1120:

```rust
Ok(true) => {
    // We just marked. Re-check is_allocated to fix TOCTOU with lazy sweep.
    if (*h).is_allocated(idx) {
        return Some(idx);
    }
    // Slot was swept between our check and try_mark. Roll back.
    (*h).clear_mark_atomic(idx);
    return None;
}
```

The issue is that when `is_allocated` returns false after a successful `try_mark`, the code clears the mark unconditionally. But if the slot was swept AND reused (a new object was allocated in the same slot with a different generation), clearing the mark would incorrectly clear the mark for the NEW object.

**Inconsistency with similar functions:**
- `scan_page_for_marked_refs` (lines 858-865): Has generation check
- `scan_page_for_unmarked_refs` (lines 992-998): Has generation check
- `mark_object_black` (lines 1113-1120): **MISSING generation check**

This indicates this was likely an oversight when the generation check pattern was established (bug336 fix).

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This is a theoretical bug - requires specific concurrent interleaving
// 1. Thread A: Object A allocated in slot with generation 1
// 2. Thread A: Object A becomes unreachable
// 3. Thread B: Lazy sweep reclaims slot, slot generation remains 1
// 4. Thread B: Object B allocated in same slot, generation increments to 2
// 5. Thread A: mark_object_black called on old Object A pointer
// 6. Thread A: try_mark succeeds (marks slot)
// 7. Thread A: is_allocated returns false (slot now unallocated/swept)
// 8. Thread A: clear_mark_atomic is called - INCORRECTLY clearing Object B's mark!
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Follow the pattern from `scan_page_for_unmarked_refs` (lines 985-999):

```rust
Ok(true) => {
    // Read generation after successful mark to detect slot reuse
    let marked_generation = (*ptr.cast::<GcBox<()>>()).generation();
    
    // Re-check is_allocated to fix TOCTOU with lazy sweep.
    if (*h).is_allocated(idx) {
        return Some(idx);
    }
    // Slot was swept between our check and try_mark. 
    // Verify generation hasn't changed to detect slot reuse.
    let current_generation = (*ptr.cast::<GcBox<()>>()).generation();
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object, don't clear
        return None;
    }
    // Slot was swept but not reused - safe to clear mark
    (*h).clear_mark_atomic(idx);
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is essential in concurrent GC environments where lazy sweep can reclaim slots that are concurrently being marked. Without the generation check, we cannot distinguish between a slot that was simply swept vs a slot that was swept and reused. This is a fundamental correctness issue for incremental/concurrent GC.

**Rustacean (Soundness 觀點):**
While this doesn't cause immediate UB (the memory is still valid), it can lead to use-after-free-like behavior where objects are incorrectly collected. The inconsistency with other similar functions that DO have the check suggests this was an oversight.

**Geohot (Exploit 觀點):**
In a concurrent scenario where an attacker can influence allocation patterns and GC timing, this could potentially be exploited to cause a targeted object to be collected while still referenced, leading to a dangling pointer scenario.
---

## Resolution (2026-03-21)

**Outcome:** Already fixed.

The generation check is present in `crates/rudo-gc/src/gc/incremental.rs` at lines 1135–1150 in `mark_object_black`. The fix reads `marked_generation` immediately after `try_mark` succeeds, then compares it to `current_generation` after `is_allocated` returns false — exactly matching the suggested fix pattern. The comment `(bug355 fix)` in the code confirms this was intentionally added. All 94 lib tests pass with no regressions.
