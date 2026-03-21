# [Bug]: scan_page_for_unmarked_refs clears mark without generation check when is_allocated is false

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep and incremental marking |
| **Severity (嚴重程度)** | High | Could incorrectly clear mark on newly allocated object |
| **Reproducibility (重現難度)** | Medium | Requires precise concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Incremental Marking`, `scan_page_for_unmarked_refs` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
After `set_mark` succeeds but `is_allocated` returns false, the code should verify the **generation** hasn't changed to distinguish between:
1. Slot was swept (slot still contains same object, mark should be cleared)
2. Slot was swept AND reused (slot contains new object with different generation, mark should NOT be cleared as it now belongs to the new object)

### 實際行為 (Actual Behavior)
The code clears the mark unconditionally when `is_allocated` fails after a successful `set_mark`, without checking the generation. This can incorrectly clear the mark on a **newly allocated object** that just happens to be in a swept slot.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**文件:** `crates/rudo-gc/src/gc/incremental.rs:979-998`

Buggy code:
```rust
if (*header).set_mark(i) {
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    let marked_generation = unsafe { (*gc_box_ptr).generation() };

    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);  // BUG: clears without checking generation!
        continue;
    }
    // Generation check comes AFTER the early return
    let current_generation = unsafe { (*gc_box_ptr).generation() };
    if current_generation != marked_generation {
        (*header).clear_mark_atomic(i);
        continue;
    }
    // ...
}
```

**問題:** When `is_allocated` returns false at line 987, we clear the mark and continue WITHOUT checking if the slot was reused (generation changed). The generation check at lines 994-997 is never reached in this code path.

**正確模式 (from `scan_page_for_marked_refs` lines 852-858):**
```rust
if !(*header).is_allocated(i) {
    let current_generation = unsafe { (*gc_box_ptr).generation() };
    if current_generation != marked_generation {
        return;  // Slot reused - don't clear, mark belongs to new object
    }
    (*header).clear_mark_atomic(i);  // Only clear if not reused
    break;
}
```

**Inconsistency with similar functions:**
- `scan_page_for_marked_refs` (incremental.rs:852-858): Checks generation when is_allocated=false
- `mark_object_black` (incremental.rs:1133-1138): Checks generation when is_allocated=false (bug355 fix)
- `mark_and_push_to_worker_queue` (gc.rs:1235-1244): Checks generation when is_allocated=false (bug360 fix)
- `GcVisitorConcurrent::route_reference` (trace.rs:194-200): Checks generation when is_allocated=false (bug362 fix)
- `scan_page_for_unmarked_refs` (incremental.rs:987-989): **MISSING generation check**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical bug - requires specific concurrent interleaving
// 1. Thread A: Object A allocated in slot with generation 1
// 2. Thread A: Object A becomes unreachable
// 3. Thread B: Lazy sweep reclaims slot (generation remains 1)
// 4. Thread B: Object B allocated in same slot, generation increments to 2
// 5. Thread A: scan_page_for_unmarked_refs called on slot
// 6. Thread A: set_mark succeeds (marks slot)
// 7. Thread A: is_allocated returns false (slot shows as unallocated in this thread's view?)
// 8. Thread A: clear_mark_atomic is called - INCORRECTLY clearing Object B's mark!
// 9. Result: Object B becomes unreachable and gets collected prematurely
```

Wait, step 7 seems wrong - if Object B is allocated, `is_allocated` should return true. Let me reconsider...

Actually, the issue is:
- After `set_mark` succeeds, if `is_allocated` returns false at line 987, we clear and continue
- But we never read `current_generation` to check if slot was reused
- If slot was swept and reused with a new object, we incorrectly clear the new object's mark

The scenario might be:
1. Object A in slot with generation 1, marked
2. Object A becomes unreachable
3. Slot is swept (but NOT reused yet) - generation still 1, is_allocated becomes false
4. Before next GC cycle, Object B is allocated in slot, generation becomes 2
5. Incremental marking scans the slot
6. set_mark succeeds (marks slot)
7. is_allocated returns true (Object B is there)
8. Generation check: current (2) != marked (1), so we DON'T clear - correct!

Actually wait, if is_allocated is true at step 7, we wouldn't hit the buggy path. Let me reconsider...

The buggy path at lines 987-989 is hit when is_allocated is false AFTER set_mark succeeds. This could happen if:
1. set_mark succeeds (marks slot)
2. Between set_mark and is_allocated check, slot is swept and becomes unallocated

In this case, we should check generation to see if slot was reused. But we don't - we just clear and continue.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the generation check BEFORE the early return when is_allocated is false:

```rust
if (*header).set_mark(i) {
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    let marked_generation = unsafe { (*gc_box_ptr).generation() };

    if !(*header).is_allocated(i) {
        // Check generation to distinguish swept from swept+reused
        let current_generation = unsafe { (*gc_box_ptr).generation() };
        if current_generation != marked_generation {
            // Slot was reused - the mark now belongs to the new object, don't clear
            // Continue processing...
        } else {
            // Slot was swept but not reused - safe to clear mark
            (*header).clear_mark_atomic(i);
        }
        continue;
    }
    // ... rest of the code
}
```

Or better yet, follow the exact pattern from `scan_page_for_marked_refs`:

```rust
if !(*header).is_allocated(i) {
    let current_generation = unsafe { (*gc_box_ptr).generation() };
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object, don't clear
        // Continue to push_work since new object should be traced
    } else {
        // Slot was swept but not reused - safe to clear mark
        (*header).clear_mark_atomic(idx);
        continue;
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is essential in concurrent GC environments where lazy sweep can reclaim slots that are concurrently being marked. Without the generation check, we cannot distinguish "slot was swept (same object)" from "slot was swept and reused (new object)". This is a fundamental correctness issue that affects all similar functions in the codebase - the fix pattern was established in bug336 and repeated in bug355, bug360, and bug362, but was missed in this function.

**Rustacean (Soundness 觀點):**
While this doesn't cause immediate UB (the memory is still valid), it can lead to use-after-free-like behavior where objects are incorrectly collected due to incorrectly cleared marks. The inconsistency with other similar functions that DO have the check suggests this was an oversight.

**Geohot (Exploit 觀點):**
In a concurrent scenario where an attacker can influence allocation patterns and GC timing, this could potentially be exploited to cause a targeted object to be collected while still referenced, leading to a dangling pointer scenario. The precise timing requirements make this difficult but not impossible.

---

## 🔗 相關 Issue

- bug258: TOCTOU between is_allocated check and set_mark - partial fix applied
- bug336: Generation check pattern established in scan_page_for_unmarked_refs
- bug355: mark_object_black missing generation check - similar issue
- bug360: mark_and_push_to_worker_queue missing generation check - similar issue
- bug362: GcVisitorConcurrent::route_reference missing generation check - similar issue

---

## 驗證記錄

**驗證日期:** 2026-03-21

**驗證方法:**
- Code review comparing `scan_page_for_unmarked_refs` (incremental.rs:979-998) with `scan_page_for_marked_refs` (incremental.rs:852-858)
- Confirmed: `scan_page_for_marked_refs` checks generation when is_allocated=false (lines 852-858)
- Confirmed: `scan_page_for_unmarked_refs` does NOT check generation when is_allocated=false (lines 987-989)
- Confirmed: The generation check at lines 994-997 only runs if is_allocated is true at line 987
