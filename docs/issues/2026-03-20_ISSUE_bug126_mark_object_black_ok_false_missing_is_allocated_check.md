# [Bug]: mark_object_black Ok(false) 路徑缺少 slot 有效性驗證 - TOCTOU 潛在問題

**Status:** Open
**Tags:** NeedsReview

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要精確的時序控制：lazy sweep 在 is_allocated 檢查和 gc_box 解引用之間執行 |
| **Severity (嚴重程度)** | Medium | 可能導致讀取無效記憶體或錯誤的對象狀態 |
| **Reproducibility (復現難度)** | Low | 需要精確控制 lazy sweep 和 marking 的執行順序 |

---

## 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_black()`, `incremental.rs:1060-1109`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

`mark_object_black()` 在解引用 `gc_box` 指標之前，應該確保 slot 仍然處於已分配狀態，防止在 TOCTOU 視窗期間 lazy sweep 回收並重用 slot 導致的 use-after-free 或讀取錯誤對象狀態。

### 實際行為 (Actual Behavior)

在 `mark_object_black()` 的 `Ok(false)` 分支中（當 `try_mark` 返回 "already marked" 時），代碼直接解引用 `gc_box` 而沒有先驗證 slot 是否仍然被分配：

```rust
// incremental.rs:1086-1095
loop {
    match (*h).try_mark(idx) {
        Ok(false) => {
            // 這裡沒有 is_allocated 檢查！
            if gc_box.is_under_construction() {  // gc_box 在 Line 1073 獲取
                return None;
            }
            // ...
        }
        Ok(true) => {
            // 這個分支有 is_allocated 檢查
            if !(*h).is_allocated(idx) {
                (*h).clear_mark_atomic(idx);
                return None;
            }
            // ...
        }
        Err(()) => {} // 重試
    }
}
```

### 對比：正確的模式

`mark_new_object_black()` 有雙重檢查保護（bug307/350 修復）：

```rust
// incremental.rs:1016-1024
if !(*header.as_ptr()).is_allocated(idx) {
    return false;
}
// 第二次檢查 - 防止 TOCTOU
if !(*header.as_ptr()).is_allocated(idx) {
    return false;
}
let gc_box = &*ptr.cast::<GcBox<()>>();  // 安全的：已經過兩次檢查
if gc_box.is_under_construction() {
    return false;
}
```

同樣的保護模式也出現在 `scan_page_for_marked_refs` 中：

```rust
Ok(false) => {
    // Already marked by another thread; move to next slot.
    // No recheck needed: we didn't mark, so nothing to roll back.
    break;  // 安全的：我們不推送這個 slot
}
```

---

## 根本原因分析 (Root Cause Analysis)

**問題本質：TOCTOU (Time-of-Check to Time-of-Use)**

1. Line 1073: `gc_box` 參考被獲取
2. Line 1078: 第一次 `is_allocated(idx)` 檢查
3. **TOCTOU 視窗**: 在 Line 1078 和 Line 1082之間，lazy sweep 可能：
   - 回收 slot（如果標記清除）
   - 重用 slot 記憶體給新對象
4. Line 1086-1088: 進入 `Ok(false)` 分支並解引用 `gc_box`

**為什麼 `Ok(true)` 分支是安全的：**
```rust
Ok(true) => {
    // 有 is_allocated 檢查
    if !(*h).is_allocated(idx) {
        (*h).clear_mark_atomic(idx);
        return None;
    }
    // ...
}
```

**為什麼 `Ok(false)` 分支有問題：**
```rust
Ok(false) => {
    // 沒有 is_allocated 檢查！
    if gc_box.is_under_construction() {  // 可能讀取無效記憶體！
        return None;
    }
    // ...
}
```

---

## 建議修復方案 (Suggested Fix / Remediation)

在 `Ok(false)` 分支中添加 `is_allocated` 檢查：

```rust
Ok(false) => {
    // 檢查 slot 是否仍然分配，防止 TOCTOU 與 lazy sweep
    if !(*h).is_allocated(idx) {
        return None;  // Slot 已被回收，停止處理
    }
    if gc_box.is_under_construction() {
        return None;
    }
    return Some(idx);
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個經典的 TOCTOU 問題。在 incremental marking 中，marking 和 sweeping 並發執行時，這種 race condition 是常見的。正確的做法是在使用任何從 GC heap 取得的指標之前，必須重新驗證 allocation 狀態。

**Rustacean (Soundness 觀點):**
這可能不是嚴格的 UB，因為 `gc_box` 只讀取內部的 atomic flags。但如果 slot 被回收並重用，`is_under_construction()` 可能返回錯誤的對象狀態。然而，由於 `is_under_construction` 是 atomic 且 `GcBox` 有 stable 地址，這可能不會導致立即的記憶體安全問題。

**George Hotz (Exploit 攻擊觀點):**
如果攻擊者能夠控制 lazy sweep 的時序，可能會：
1. 在 `Ok(false)` 路徑中強制 slot 回收
2. 將 slot 重用為具有特定 `is_under_construction` 狀態的對象
3. 導致 GC 錯誤地跳過標記或錯誤地標記對象

---

## 備註

- 這個問題與 bug291、bug272 等其他 TOCTOU 修復相關
- `mark_new_object_black` 已經有 bug307/350 的雙重檢查修復
- `scan_page_for_unmarked_refs` 中的 `Ok(false)` 路徑是安全的，因為它不推送任何東西

---

## 驗證步驟

1. 確認 `mark_object_black` 在 `Ok(false)` 路徑中缺少 `is_allocated` 檢查
2. 驗證 `mark_new_object_black` 有雙重檢查保護
3. 確認這確實是一個需要在 `Ok(false)` 路徑中添加 `is_allocated` 檢查的 bug
