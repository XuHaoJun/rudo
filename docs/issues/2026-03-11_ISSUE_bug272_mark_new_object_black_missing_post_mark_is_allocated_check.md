# [Bug]: mark_new_object_black 缺少 set_mark 後的 is_allocated 檢查 - 與 mark_object_black 行為不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 lazy sweep 和標記並發執行時觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致標記已釋放的 slot，潛在 UAF |
| **Reproducibility (難度)** | Medium | 需要並發場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_new_object_black`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為

`mark_new_object_black` 應該與 `mark_object_black` 有一致的行為，都需要在標記成功後再次檢查 `is_allocated` 以防止 TOCTOU race。

### 實際行為

`mark_new_object_black` 在調用 `set_mark` 後沒有再次檢查 `is_allocated`，而 `mark_object_black` 有這個檢查。

### 程式碼位置

`gc/incremental.rs` 第 983-997 行：

```rust
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            if !(*header.as_ptr()).is_allocated(idx) {  // 第一次檢查
                return false;
            }
            if !(*header.as_ptr()).is_marked(idx) {
                (*header.as_ptr()).set_mark(idx);  // 標記
                // BUG: 缺少第二次 is_allocated 檢查！
                return true;
            }
        }
    }
    false
}
```

### 對比：mark_object_black 的正確實現

`gc/incremental.rs` 第 1012-1037 行：

```rust
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    // ...
    if !(*h).is_allocated(idx) {  // 第一次檢查
        return None;
    }
    loop {
        match (*h).try_mark(idx) {
            Ok(false) => return Some(idx),
            Ok(true) => {
                // 有第二次檢查！
                if (*h).is_allocated(idx) {  // <-- 關鍵差異
                    return Some(idx);
                }
                // Slot 被 sweep 了，回滾標記
                (*h).clear_mark_atomic(idx);
                return None;
            }
            Err(()) => {}
        }
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

Bug108 修復了初始的 `is_allocated` 檢查，但遺漏了標記後的第二次檢查。`mark_object_black` 使用 `try_mark` + 重新檢查的模式，但 `mark_new_object_black` 使用簡單的 `set_mark` 然後返回，沒有重新驗證。

**Race 條件說明**:
1. Thread A 調用 `mark_new_object_black(ptr)`
2. Line 987: `is_allocated(idx)` 返回 true - slot 有效
3. **Race Window**: Thread B 在此時 sweep 了 slot
4. Line 991: `set_mark(idx)` - 標記已釋放的 slot
5. Line 992: 返回 true - 錯誤地標記了無效物件

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發場景：
1. 一個執行緒正在分配新物件並調用 `mark_new_object_black`
2. 另一個執行緒同時進行 lazy sweep
3. 在 `is_allocated` 檢查和 `set_mark` 之間，slot 被 sweep 並重用

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `mark_new_object_black` 中添加標記後的 `is_allocated` 檢查：

```rust
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            if !(*header.as_ptr()).is_allocated(idx) {
                return false;
            }
            if !(*header.as_ptr()).is_marked(idx) {
                (*header.as_ptr()).set_mark(idx);
                // 添加第二次檢查以修復 TOCTOU
                if !(*header.as_ptr()).is_allocated(idx) {
                    (*header.as_ptr()).clear_mark_atomic(idx);
                    return false;
                }
                return true;
            }
        }
    }
    false
}
```

或者使用與 `mark_object_black` 相同的模式，使用 `try_mark` 並在成功後檢查。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Black allocation 是 SATB 優化，新物件應該被視為 live
- 問題是：如果 slot 在標記後被 sweep 且重用，會導致標記無效的元數據
- `mark_object_black` 已經有這個保護，`mark_new_object_black` 應該一致

**Rustacean (Soundness 觀點):**
- 這是防禦性編程問題
- 雖然當前實現中 allocation 和 sweep 可能是串行的，但代碼應該對未來可能的並發場景保持安全

**Geohot (Exploit 攻擊觀點):**
- 如果未來實現並發 sweep，這裡可能成為 UAF 的源頭
- 攻擊者可能嘗試在 slot 重用時干擾標記狀態

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
