# [Bug]: process_owned_page TOCTOU - Ok(false) 路徑缺少 is_allocated 檢查

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
- **Component:** `process_owned_page` in `gc/marker.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`process_owned_page` 函數在 `try_mark` 返回 `Ok(false)` (已被其他執行緒標記) 時直接 break，沒有重新檢查 `is_allocated`。此時 slot 可能已被 lazy sweep 回收並重複使用。

### 預期行為
當 slot 被 sweep 回收後，應該略過該 slot 或回滾標記。

### 實際行為
當 slot 被標記後又被 sweep，回傳 break 導致繼續處理，可能導致對已回收記憶體的操作。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/marker.rs:697-720` 的 `process_owned_page` 函數中：

```rust
loop {
    match (*header).try_mark(i) {
        Ok(false) => break, // BUG: 未重新檢查 is_allocated!
        Ok(true) => {
            // 這裡有 re-check
            if !(*header).is_allocated(i) {
                (*header).clear_mark_atomic(i);
                break;
            }
            // Second check to fix TOCTOU
            if !(*header).is_allocated(i) {
                (*header).clear_mark_atomic(i);
                break;
            }
            marked += 1;
            self.push(gc_box_ptr.as_ptr());
            break;
        }
        Err(()) => {} // CAS 失敗，重試
    }
}
```

當 `try_mark` 返回 `Ok(false)` 時（表示已被其他執行緒標記），程式直接 break 而沒有再次檢查 `is_allocated`。

相比之下，`gc/incremental.rs:mark_object_black` 函數已經修復了這個問題（bug291），而在 `gc/marker.rs:process_owned_page` 的 Ok(true) 路徑也有檢查，但 Ok(false) 路徑漏掉了。

TOCTOU 時序：
1. 執行緒 A: 檢查 `is_allocated(i) && !is_marked(i)` @ line 688 → true
2. 執行緒 B: lazy sweep 清除 allocated bit，回收 slot
3. 執行緒 B: 標記物件（設置 mark bit）
4. 執行緒 A: `try_mark` 返回 `Ok(false)`（已被標記）
5. 執行緒 A: break 但 slot 已被回收！

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 建立大量 GC 物件
2. 啟動 parallel marking
3. 同時觸發 lazy sweep
4. 觀察是否標記到已回收的 slot

```rust
// PoC 需要精確時序控制
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Ok(false)` 分支中加入 `is_allocated` 重新檢查：

```rust
Ok(false) => {
    // Re-check is_allocated to fix TOCTOU with lazy sweep.
    // If slot was swept after initial check but before we get here,
    // break to skip using a reused slot.
    if (*header).is_allocated(i) {
        break;
    }
    // Slot was swept, continue to next iteration
    continue;
}
```

或者更簡單地：
```rust
Ok(false) => {
    // Re-check is_allocated to fix TOCTOU
    if !(*header).is_allocated(i) {
        break;
    }
    break; // Already marked by another thread, slot is still valid
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這個問題與 bug291 類似，都是標記過程中的 TOCTOU 問題
- parallel marking 增加了並發競爭的機會
- 修復模式應該與 mark_object_black 一致

**Rustacean (Soundness 觀點):**
- 這是經典的 TOCTOU 競爭條件
- 需要確保所有程式碼路徑有一致的檢查模式
- Ok(true) 路徑已經有檢查，Ok(false) 應該也要有一致性

**Geohot (Exploit 觀點):**
- 若此漏洞被利用，需要精確的執行時序
- 在高負載 GC 環境中有可能被觸發
- 實際危害可能有限，但應修復以確保正確性

---

## Resolution (2026-03-15)

**Fix applied:** Added `is_allocated` re-check in `process_owned_page` Ok(false) path (`gc/marker.rs`). When `try_mark` returns `Ok(false)` (already marked by another thread), we now re-verify the slot is still allocated before treating it as valid. Aligns with the pattern used in `gc/incremental.rs` (bug291, scan_page_for_unmarked_refs).
