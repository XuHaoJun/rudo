# [Bug]: mark_object_black TOCTOU when is_allocated returns true

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep and marking with slot reuse |
| **Severity (嚴重程度)** | `Medium` | Could cause marking wrong object or stale mark persistence |
| **Reproducibility (復現難度)** | `Medium` | Requires specific concurrent interleaving |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Incremental Marking`, `mark_object_black` in `gc/incremental.rs`
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

When `mark_object_black` successfully marks an object via `try_mark` and `is_allocated` returns true, the function returns `Some(idx)` without verifying the generation hasn't changed. If the slot was swept and immediately reused by a new object between `try_mark` and the `is_allocated` check, the function incorrectly returns success for the new object's slot.

### 預期行為 (Expected Behavior)
After `try_mark` succeeds and `is_allocated` returns true, the code should verify the **generation** hasn't changed to distinguish between:
1. Slot still contains the same object (mark is valid)
2. Slot was swept AND reused (slot contains new object with different generation, mark belongs to old object)

### 實際行為 (Actual Behavior)
The code returns `Some(idx)` immediately when `is_allocated` is true, without checking if the generation changed. This can cause the mark to be associated with the wrong object.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `crates/rudo-gc/src/gc/incremental.rs`, lines 1132-1138:

```rust
Ok(true) => {
    // Read generation after successful mark to detect slot reuse (bug355 fix).
    let marked_generation = (*gc_box).generation();
    // We just marked. Re-check is_allocated to fix TOCTOU with lazy sweep.
    if (*h).is_allocated(idx) {
        return Some(idx);  // BUG: No generation check here!
    }
    // ... generation check only happens when is_allocated=false
}
```

The bug355 fix added a generation check when `is_allocated` returns false, but the complementary case where `is_allocated` returns true was not addressed. When `is_allocated(idx)` is true, the code should still verify the generation matches to detect slot reuse.

**Race scenario:**
1. Object A (gen=5) is marked via `try_mark` (succeeds)
2. Object A's generation is read: `marked_generation = 5`
3. Slot is swept - Object A is dropped, slot becomes free
4. New object B (gen=6, also live) is allocated in the same slot
5. At line 1136: `is_allocated(idx)` returns `true` (B is there)
6. Function returns `Some(idx)` - **WRONG!** We marked A but returned success for B's slot

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical bug - requires specific concurrent interleaving
// 1. Thread A: Object A allocated in slot with generation 5
// 2. Thread A: Object A becomes unreachable
// 3. Thread B: Lazy sweep reclaims slot (Object A dropped)
// 4. Thread B: Object B allocated in same slot (generation 6)
// 5. Thread A: mark_object_black called on old Object A pointer
// 6. Thread A: try_mark succeeds (marks slot)
// 7. Thread A: marked_generation = 5 is read
// 8. Thread A: is_allocated returns true (Object B is in slot)
// 9. Thread A: returns Some(idx) WITHOUT checking generation mismatch!
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation check before returning `Some(idx)` when `is_allocated` is true:

```rust
Ok(true) => {
    let marked_generation = (*gc_box).generation();
    if (*h).is_allocated(idx) {
        // Also check generation to detect slot reuse
        let current_generation = (*gc_box).generation();
        if current_generation != marked_generation {
            // Slot was reused - mark belongs to new object
            return None;
        }
        return Some(idx);
    }
    // Slot was swept - check generation to distinguish swept from swept+reused
    let current_generation = (*gc_box).generation();
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object, don't clear.
        return None;
    }
    // Slot was swept but not reused - safe to clear mark.
    (*h).clear_mark_atomic(idx);
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is essential in concurrent GC environments where lazy sweep can reclaim and reuse slots while marking is in progress. Without this check, we cannot distinguish "slot still contains marked object" from "slot was reused by new object". This is a fundamental correctness issue for incremental/concurrent GC.

**Rustacean (Soundness 觀點):**
This doesn't cause immediate UB (memory is valid), but can lead to incorrect GC behavior where marks are associated with wrong objects. The inconsistency with the `is_allocated=false` path (which DOES have generation check) suggests this was an oversight.

**Geohot (Exploit 觀點):**
In a concurrent scenario, an attacker might influence allocation patterns and GC timing to cause mark confusion between objects. While not a direct UAF, this could lead to subtle memory corruption in long-running GC-intensive workloads.

---

## Resolution (2026-03-28)

**Outcome:** Fixed in `gc/incremental.rs` `mark_object_black`: on `try_mark` success, when `is_allocated(idx)` is true, the code compares `marked_generation` to `current_generation` and returns `None` on mismatch (comment `bug399 fix`).
