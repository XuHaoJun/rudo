# [Bug]: scan_page_for_unmarked_refs redundant second is_allocated check (bug258 fix incorrectly applied)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 程式碼沉默，不影響功能（dead code） |
| **Severity (嚴重程度)** | Low | 不導致記憶體損壞，只是多餘的檢查 |
| **Reproducibility (Reproducibility)** | N/A | 程式碼結構問題，無需復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_unmarked_refs` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`scan_page_for_unmarked_refs` 函數應該只有一個 `is_allocated` 檢查（確保 slot 在 `set_mark` 後仍然有效），然後執行 `push_work`。

### 實際行為 (Actual Behavior)

存在兩個連續的 `is_allocated` 檢查，兩者之間**沒有任何程式碼**：

```rust
if !(*header).is_allocated(i) {      // 第一個檢查 (line 972)
    (*header).clear_mark_atomic(i);
    continue;
}
// 這裡沒有程式碼！
if !(*header).is_allocated(i) {      // 第二個檢查 (line 978) - 立即在第一個之後
    (*header).clear_mark_atomic(i);
    continue;
}
```

**問題：**
1. 兩個檢查**完全相同**
2. 如果第一個檢查通過（`is_allocated` 返回 true），第二個檢查**必然也通過**
3. 第二個檢查是 **dead code**，提供零額外保護

**Race window 分析：**
- Bug258 描述的 TOCTOU 是「在第一次 `is_allocated` 檢查和 `push_work` 之间存在 race window」
- 第一個檢查在 line 972，`push_work` 在 line 988
- 但第二個檢查在 line 978，**立郎在第一個檢查之後**，不是「在 push_work 之前」

---

## 🔬 根本原因分析 (Root Cause Analysis)

Bug258 的修復被錯誤地套用。第二個檢查被放置在第一個檢查**立即之後**，而不是在 `push_work` **之前**。

正確的模式應該是：
```rust
if (*header).set_mark(i) {
    // 第一次檢查：確保 set_mark 後 slot 仍然有效
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        continue;
    }
    
    // ... 其他操作（ gc_box_ptr 創建等）...
    
    // 第二次檢查（应该在 push_work 之前）：確保到 push_work 前 slot 仍然有效
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        continue;
    }
    
    ptr.push_work(gc_box);  // <-- 第二次檢查應該在這裡之前
}
```

但當前代碼的問題是第二個檢查緊接在第一個之後，而之間只有一個右花括號。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

N/A - 這是程式碼結構問題，不需要 PoC。分析如下：

```rust
// 第一次檢查通過的情況：
if !(*header).is_allocated(i) {  // false - is_allocated 返回 true
    ...
}
if !(*header).is_allocated(i) {  // 仍然是 false - is_allocated 沒有改變
    ...  // 永遠不會進入這個 block
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**選項 1：移除第二個檢查（如果它真的是錯誤地添加的）**

Bug258 的修復可能已經足夠。第一個檢查（line 972）已經處理了 `set_mark` 後 slot 被 sweep 的情況。如果需要第二個檢查來防止「第一次檢查到 push_work 之间」的 race，它應該在 `push_work` 之前，而不是立即在第一個檢查之後。

**選項 2：將第二個檢查移到 push_work 之前（如果 bug258 修復需要）**

```rust
if (*header).set_mark(i) {
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        continue;
    }
    
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    if let Some(gc_box) = NonNull::new(gc_box_ptr) {
        // 在這裡添加第二次檢查（push_work 之前）
        if !(*header).is_allocated(i) {
            (*header).clear_mark_atomic(i);
            continue;
        }
        let ptr = IncrementalMarkState::global();
        ptr.push_work(gc_box);
    }
}
```

**分析：**

選項 1 可能是正確的，因為：
1. `gc_box_ptr` 的創建（line 985）只是一個指標投射，不會接觸 slot 內容
2. 如果 slot 在第一次檢查後被 sweep，`gc_box_ptr` 仍然指向正確的記憶體位址（只是該位址現在屬於另一個物件）
3. `push_work` 只是將指標推入佇列，不會立即解引用

所以第二個檢查可能是多餘的。建議採用選項 1，移除第二個檢查。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這看起來像是錯誤的程式碼複製粘貼，或者對 bug258 的過度修復。第一個檢查已經處理了 set_mark 後的 TOCTOU。第二個檢查立即跟在第一個後面，邏輯上不可能看到不同的結果。

**Rustacean (Soundness 觀點):**
這不是安全問題，只是多餘的檢查。兩個連續的相同檢查，第二個永遠不會有不同的結果。

**Geohot (Exploit 攻擊觀點):**
沒有實際的攻擊面。這個錯誤的檢查不會導致任何可利用的漏洞。

---

## Related Issues

- bug258: Original issue documenting the TOCTOU between is_allocated check and push_work
- bug175: set_mark and is_allocated TOCTOU (fixed earlier)

---

## 修復記錄

**Date:** 2026-03-20

**Status:** Issue created but fix NOT applied - code still contains the bug!

The issue was marked Fixed but the actual code was never modified. The redundant second `is_allocated` check still exists at lines 992-997.

## Resolution (2026-03-21)

**Outcome:** Fixed via commit sequence.

- `4cb3261` ("fix(incremental): remove dead code - redundant second is_allocated check in scan_page_for_unmarked_refs") removed the redundant consecutive duplicate.
- `d2a2ebb` ("fix(incremental): add second is_allocated re-check in scan_page_for_unmarked_refs") then added the check back in the *correct* position — just before `push_work` (Option 2 from the suggested fix).
- `11965b0` further added generation checks between the two `is_allocated` checks.

Current code at `gc/incremental.rs` has two `is_allocated` checks with substantial generation-check logic between them; the second is correctly placed immediately before `ptr.push_work(gc_box)`. The original "redundant consecutive" pattern no longer exists.
