# [Bug]: mark_and_push_to_worker_queue missing generation check after try_mark

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep and parallel marking |
| **Severity (嚴重程度)** | `High` | Could incorrectly clear mark on newly allocated object |
| **Reproducibility (重現難度)** | `Medium` | Requires precise concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Parallel Marking`, `mark_and_push_to_worker_queue` in `gc/gc.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.x`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
After `try_mark` succeeds but `is_allocated` returns false, the code should verify the **generation** hasn't changed to distinguish between:
1. Slot was swept (slot still contains same object, mark should be cleared)
2. Slot was swept AND reused (slot contains new object with different generation, mark should NOT be cleared)

### 實際行為 (Actual Behavior)
The code clears the mark unconditionally when `is_allocated` fails after a successful `try_mark`, without checking the generation. This can incorrectly clear the mark on a **newly allocated object** that just happens to be in a swept slot.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `crates/rudo-gc/src/gc/gc.rs`, lines 1235-1241:

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    break; // We marked, slot still valid
}
```

The issue: When `is_allocated` returns false after successful `try_mark`, the code clears the mark unconditionally. But if the slot was swept AND reused (a new object was allocated in the same slot with a different generation), clearing the mark would incorrectly clear the mark for the NEW object.

**Inconsistency with similar functions:**
- `scan_page_for_unmarked_refs` (incremental.rs:979-1000): Has generation check (bug336 fix)
- `scan_page_for_marked_refs` (incremental.rs:843-852): Has generation check (bug336 fix)
- `mark_object_black` (incremental.rs:1113-1120): MISSING generation check (bug355)
- `mark_and_push_to_worker_queue` (gc.rs:1235-1241): **MISSING generation check**

The bug295 fix added the `try_mark + is_allocated recheck` pattern but missed the generation check that bug336 introduced in other similar functions.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical bug - requires specific concurrent interleaving
// 1. Thread A: Object A allocated in slot with generation 1
// 2. Thread A: Object A becomes unreachable
// 3. Thread B: Lazy sweep reclaims slot (generation remains 1)
// 4. Thread B: Object B allocated in same slot, generation increments to 2
// 5. Thread A: mark_and_push_to_worker_queue called on old Object A pointer
// 6. Thread A: try_mark succeeds (marks slot with generation 2's mark bit)
// 7. Thread A: is_allocated returns false (slot shows as unallocated)
// 8. Thread A: clear_mark_atomic is called - INCORRECTLY clearing Object B's mark!
// 9. Result: Object B becomes unreachable and gets collected
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Follow the pattern from `scan_page_for_unmarked_refs` (incremental.rs:979-1000):

```rust
Ok(true) => {
    // Read generation after successful mark to detect slot reuse (bug336)
    let marked_generation = (*gc_box.as_ptr()).generation();
    
    // Re-check is_allocated to fix TOCTOU with lazy sweep (bug295).
    if !(*header.as_ptr()).is_allocated(idx) {
        // Slot was swept. Verify if it was reused by checking generation.
        let current_generation = (*gc_box.as_ptr()).generation();
        if current_generation != marked_generation {
            // Slot was reused - the mark now belongs to the new object, don't clear
            return;
        }
        // Slot was swept but not reused - safe to clear mark
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    break; // We marked, slot still valid
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is essential in concurrent GC environments where lazy sweep can reclaim slots that are concurrently being marked. Without the generation check, we cannot distinguish between a slot that was simply swept vs a slot that was swept and reused. This is the same bug355 pattern but in the parallel marking path.

**Rustacean (Soundness 觀點):**
While this doesn't cause immediate UB (the memory is still valid), it can lead to use-after-free-like behavior where objects are incorrectly collected due to incorrectly cleared marks. The inconsistency with other similar functions that DO have the check (scan_page_for_unmarked_refs, scan_page_for_marked_refs) suggests this was an oversight from the bug295 fix.

**Geohot (Exploit 觀點):**
In a concurrent scenario, an attacker could influence allocation patterns and GC timing to cause a targeted object to be collected while still referenced, leading to a dangling pointer scenario. This is a real memory safety concern.

---

## 🔗 相關 Issue

- bug295: TOCTOU between is_allocated check and set_mark - partial fix applied
- bug336: Generation check to detect slot reuse - pattern established
- bug355: mark_object_black missing generation check - similar issue in incremental.rs

---

## 驗證記錄

**驗證日期:** 2026-03-21

**驗證方法:**
- Code review comparing `mark_and_push_to_worker_queue` (gc.rs:1214-1250) with `scan_page_for_unmarked_refs` (incremental.rs:960-1011)
- Confirmed: `scan_page_for_unmarked_refs` has generation check (lines 979-993)
- Confirmed: `mark_and_push_to_worker_queue` is MISSING generation check
- Pattern is identical to bug355 but in different function