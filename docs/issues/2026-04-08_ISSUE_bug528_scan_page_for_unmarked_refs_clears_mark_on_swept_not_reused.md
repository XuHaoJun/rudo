# [Bug]: scan_page_for_unmarked_refs clears mark on swept-but-not-reused slot

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Lazy sweep concurrent with incremental marking could trigger this |
| **Severity (嚴重程度)** | `Medium` | Live object incorrectly swept, causing UAF |
| **Reproducibility (復現難度)** | `Medium` | Requires precise timing between lazy sweep and incremental mark |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_unmarked_refs` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When a slot is swept (allocated=false) but NOT reused (generation unchanged), the mark should NOT be cleared because we cannot distinguish swept from swept+reused using only `is_allocated`.

### 實際行為 (Actual Behavior)
In `scan_page_for_unmarked_refs`, when `is_allocated` returns false after a successful `try_mark`, the code immediately clears the mark without checking if the slot was swept but not reused.

### 對比 `scan_page_for_marked_refs`
The sibling function `scan_page_for_marked_refs` has the correct logic (lines 856-860):
```rust
let current_generation = unsafe { (*gc_box_ptr).generation() };
if current_generation != marked_generation {
    // Slot was reused - the mark now belongs to the new object, don't clear.
    break;
}
```

`scan_page_for_unmarked_refs` is missing this generation check before clearing the mark.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `scan_page_for_unmarked_refs` (around line 1016-1018):
```rust
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);  // BUG: clears even when slot just swept, not reused
    break;
}
```

When a slot is swept by lazy sweep and NOT immediately reused:
1. `try_mark` succeeds (we mark it)
2. `is_allocated` returns false (slot was swept)
3. Code clears the mark - WRONG! If generation unchanged, slot was swept but NOT reused

This is incorrect because clearing the mark on a live (but swept-and-not-reused) slot causes the GC to think the object is dead when it's actually still live.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)
```rust
// Requires concurrent lazy sweep and incremental marking
// Precise timing needed - difficult to reproduce reliably
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation check before clearing the mark, similar to `scan_page_for_marked_refs`:

```rust
if !(*header).is_allocated(i) {
    // Check if slot was swept+reused (generation changed) or just swept (no reuse)
    let current_generation = unsafe { (*gc_box_ptr).generation() };
    if current_generation != marked_generation {
        // Slot was reused - mark belongs to new object, don't clear
        break;
    }
    // Slot was swept but not reused - safe to clear mark
    (*header).clear_mark_atomic(i);
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is critical for SATB consistency. Without it, we can incorrectly mark an object as dead during incremental marking when the slot was swept but immediately reallocated to a different object.

**Rustacean (Soundness 觀點):**
This is a soundness bug - it can cause UAF when a live object's slot is incorrectly swept.

**Geohot (Exploit 觀點):**
Timing-dependent but exploitable if achieved. Could be triggered by carefully timed allocations to cause the sweep during incremental marking.