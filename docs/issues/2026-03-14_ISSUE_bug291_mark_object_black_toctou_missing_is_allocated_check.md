# [Bug]: mark_object_black TOCTOU - 已在其他地方修復但未同步

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發：標記執行中 + lazy sweep 執行中 |
| **Severity (嚴重程度)** | Medium | 可能導致已回收物件被錯誤標記為 live |
| **Reproducibility (復現難度)** | High | 需要精確的執行時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_black` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`mark_object_black` 函數在 `try_mark` 返回 `Ok(false)` (已標記) 時不回傳 `None`，而是直接回傳 `Some(idx)`，但此時 slot 可能已被 lazy sweep 回收並重複使用。

### 預期行為
當 slot 被 sweep 回收後，應該回傳 `None` 表示標記失敗。

### 實際行為
當 slot 被標記後又被 sweep，回傳 `Some(idx)` 可能導致對已回收記憶體的操作。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/incremental.rs:1012-1037` 的 `mark_object_black` 函數中：

```rust
loop {
    match (*h).try_mark(idx) {
        Ok(false) => return Some(idx), // BUG: 未重新檢查 is_allocated!
        Ok(true) => {
            // 這裡有 re-check
            if (*h).is_allocated(idx) {
                return Some(idx);
            }
            (*h).clear_mark_atomic(idx);
            return None;
        }
        Err(()) => {} // CAS 失敗，重試
    }
}
```

當 `try_mark` 返回 `Ok(false)` 時（表示已被其他執行緒標記），程式直接回傳 `Some(idx)` 而沒有再次檢查 `is_allocated`。

相比之下，在 `scan_page_for_marked_refs` 中有類似的模式，但該函數在 line 808 有預先檢查 `is_allocated(i) && !(*header).is_marked(i)`，而 `mark_object_black` 在 line 1018 檢查後，進入 loop 內的 `Ok(false)` 路徑沒有再次檢查。

TOCTOU 時序：
1. 執行緒 A: 檢查 `is_allocated(idx)` @ line 1018 → true
2. 執行緒 B: lazy sweep 清除 allocated bit，回收 slot
3. 執行緒 B: 標記物件（設置 mark bit）
4. 執行緒 A: `try_mark` 返回 `Ok(false)`（已被標記）
5. 執行緒 A: 回傳 `Some(idx)` 但 slot 已被回收！

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 建立大量 GC 物件
2. 啟動 incremental marking
3. 同時觸發 lazy sweep
4. 從多執行緒呼叫 `GcCell::borrow_mut()` 寫入 barrier

```rust
// PoC 需要精確時序控制
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Ok(false)` 分支中加入 `is_allocated` 重新檢查：

```rust
Ok(false) => {
    // 重新檢查 is_allocated 以修復 TOCTOU
    if (*h).is_allocated(idx) {
        return Some(idx);
    }
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Lazy sweep 與 incremental marking 的交互是常見的 GC 實現挑戰
- 需要在標記速度與正確性之間取得平衡

**Rustacean (Soundness 觀點):**
- 這是經典的 TOCTOU 競爭條件
- 需要確保所有程式碼路徑有一致的檢查模式

**Geohot (Exploit 觀點):**
- 若此漏洞被利用，需要精確的執行時序
- 實際危害可能有限，但應修復以確保正確性

---

## Resolution (2026-03-15)

**Fix verified:** The `Ok(false)` branch in `mark_object_black` (`gc/incremental.rs`) already includes the `is_allocated` re-check (lines 1071–1077). The fix was applied previously; the issue file status is updated to Fixed.
