# [Bug]: scan_page_for_unmarked_refs incorrectly clears mark when slot is reused

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep during FinalMark and slot reuse |
| **Severity (嚴重程度)** | High | Could cause live objects to be incorrectly swept |
| **Reproducibility (重現難度)** | Medium | Requires concurrent stress test |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `scan_page_for_unmarked_refs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When `scan_page_for_unmarked_refs` detects that a slot was reused (generation mismatch after successful mark), it should NOT clear the mark. The mark now belongs to the new object in the reused slot.

This matches `mark_object_black` behavior (lines 1143-1146) which correctly does not clear when slot is reused.

### 實際行為 (Actual Behavior)

`scan_page_for_unmarked_refs` (lines 1007-1009) incorrectly clears the mark when generation mismatch is detected:

```rust
// Lines 1007-1009 - BUG: incorrectly clears mark
if current_generation != marked_generation {
    (*header).clear_mark_atomic(i);  // WRONG!
    break;
}
```

### 對比：`mark_object_black` 正確行為 (lines 1143-1150):

```rust
// Lines 1143-1150 - CORRECT: doesn't clear when slot reused
let current_generation = (*gc_box).generation();
if current_generation != marked_generation {
    // Slot was reused - the mark now belongs to the new object, don't clear.
    return None;  // DON'T clear
}
// Slot was swept but not reused - safe to clear mark.
(*h).clear_mark_atomic(idx);
return None;
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

When a slot is reused during lazy sweep while FinalMark is running:

1. Old object A in slot, is_marked = false
2. `scan_page_for_unmarked_refs` calls `try_mark`, is_marked becomes true
3. Slot is swept and reused by new object B (generation changes)
4. Generation check sees mismatch (B's generation ≠ A's generation)
5. BUG: Code clears the mark that now belongs to B
6. B may lose its only mark and be incorrectly swept

The comment at line 1003-1005 says "Verify generation hasn't changed... we should skip this object" but it incorrectly clears the mark. The comment and code are inconsistent.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires concurrent stress test with:
// 1. Heavy allocation pressure causing slot reuse
// 2. FinalMark phase running while lazy sweep occurs
// 3. Observing objects that should be live being incorrectly collected
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change lines 1007-1009 from:
```rust
if current_generation != marked_generation {
    (*header).clear_mark_atomic(i);
    break;
}
```

To match `mark_object_black`:
```rust
if current_generation != marked_generation {
    // Slot was reused - the mark now belongs to the new object, don't clear.
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
When a slot is reused, the mark bit belongs to the new object. Clearing it would cause the new object to potentially lose its only mark and be collected prematurely. The `mark_object_black` function correctly handles this case - `scan_page_for_unmarked_refs` should match.

**Rustacean (Soundness 觀點):**
Incorrectly clearing a mark on a live object could lead to use-after-free if the object is collected while still reachable through other references that weren't scanned.

**Geohot (Exploit 攻擊觀點):**
An attacker controlling allocation patterns could trigger this race to cause memory corruption or leak sensitive data from incorrectly collected objects.

---

## 驗證記錄

**驗證日期:** 2026-03-23

**驗證方法:**
- Code comparison between `scan_page_for_unmarked_refs` (lines 1007-1009) and `mark_object_black` (lines 1143-1150)
- Confirmed inconsistency: same scenario handled differently
- `mark_object_black` is the correct pattern (doesn't clear on slot reuse)
