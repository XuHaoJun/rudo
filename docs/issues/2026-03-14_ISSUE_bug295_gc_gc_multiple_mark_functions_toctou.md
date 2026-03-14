# [Bug]: Multiple mark functions in gc/gc.rs have TOCTOU between is_allocated check and set_mark

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發：標記執行中 + lazy sweep 執行中 |
| **Severity (嚴重程度)** | High | 可能導致錯誤標記已回收並重複使用的 slot |
| **Reproducibility (重現難度)** | High | 需要精確的執行時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Multiple functions in `gc/gc.rs`:
  - `mark_and_push_to_worker_queue` (lines 1219-1235)
  - `scan_segment_for_roots` (lines 2060-2088)  
  - `mark_object` (lines 2355-2381)
  - `mark_and_trace_incremental` (lines 2383-2413)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

多個標記函數在檢查 `is_allocated` 和調用 `set_mark` 之間存在 TOCTOU 競爭條件。此時 slot 可能已被 lazy sweep 回收並重複使用。

### 預期行為
當 slot 被 sweep 回收後，應該略過該 slot 或使用 try_mark 模式確保原子性。

### 實際行為
當 slot 被標記前被 sweep，回傳導致繼續處理，可能導致對已回收記憶體的操作。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 1. `mark_and_push_to_worker_queue` (lines 1224-1229)

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
if !(*header.as_ptr()).is_marked(idx) {
    (*header.as_ptr()).set_mark(idx);  // TOCTOU: slot could be swept between check and set_mark
}
```

### 2. `scan_segment_for_roots` (lines 2071-2079)

```rust
if !(*header.as_ptr()).is_allocated(index) {
    return;
}

if (*header.as_ptr()).is_marked(index) {
    return;
}

(*header.as_ptr()).set_mark(index);  // TOCTOU: slot could be swept between check and set_mark
```

### 3. `mark_object` (lines 2367-2373)

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
if (*header.as_ptr()).is_marked(idx) {
    return;
}
(*header.as_ptr()).set_mark(idx);  // TOCTOU: slot could be swept between check and set_mark
```

### 4. `mark_and_trace_incremental` (lines 2400-2406)

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
if (*header.as_ptr()).is_marked(idx) {
    return;
}
(*header.as_ptr()).set_mark(idx);  // TOCTOU: slot could be swept between check and set_mark
```

問題在於 is_allocated 檢查和 set_mark 調用之間存在 TOCTOU window：

1. 執行緒 A: 檢查 `is_allocated(idx)` → true
2. 執行緒 B: lazy sweep 清除 allocated bit，回收 slot
3. 執行緒 B: 在同一 slot 分配新物件
4. 執行緒 A: 檢查 `is_marked(idx)` → false (新物件)
5. 執行緒 A: 調用 `set_mark(idx)` → 錯誤地標記了新物件！

正確的模式應該使用 `try_mark` + recheck 模式，就像 `process_owned_page` 函數中的實現一樣。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 建立大量 GC 物件
2. 啟動 parallel marking
3. 同時觸發 lazy sweep
4. 觀察是否標記到已回收並重複使用的 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

所有受影響的函數都應該使用 try_mark 模式來確保原子性：

```rust
// 替代方案 1: 使用 try_mark 模式
loop {
    match (*header).try_mark(idx) {
        Ok(false) => {
            // 已被其他執行緒標記
            // 重新檢查 is_allocated
            if !(*header).is_allocated(idx) {
                break; // slot 被回收，跳過
            }
            break; // 已被標記，slot 有效
        }
        Ok(true) => {
            // 我們標記了
            // 重新檢查 is_allocated
            if !(*header).is_allocated(idx) {
                (*header).clear_mark_atomic(idx);
                break; // slot 被回收，回滾標記
            }
            // ... trace object
            break;
        }
        Err(()) => {} // CAS 失敗，重試
    }
}

// 替代方案 2: 在 set_mark 前再次檢查 is_allocated
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
if (*header.as_ptr()).is_marked(idx) {
    return;
}
// 添加：再次檢查 is_allocated
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
(*header.as_ptr()).set_mark(idx);
```

參考 `process_owned_page` 函數 (gc/marker.rs lines 697-720) 和 `scan_page_for_marked_refs` 函數 (gc/incremental.rs lines 810-845) 的正確實現模式。

這些函數已經使用 try_mark + recheck 模式來修復類似的 TOCTOU 問題。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這個問題與 bug291、bug292、bug294 類似，都是標記過程中的 TOCTOU 問題
- 這是經典的標記與 sweep 競爭條件
- 應該使用 try_mark 模式來確保原子性

**Rustacean (Soundness 觀點):**
- 這是經典的 TOCTOU 競爭條件
- 可能導致錯誤地標記新分配的物件
- 影響 GC 的正確性

**Geohot (Exploit 攻擊觀點):**
- 若此漏洞被利用，需要精確的執行時序
- 攻擊者可能利用此漏洞影響 GC 行為

---

## 🔗 相關 Issue

- bug291: mark_object_black TOCTOU - 修復模式範例
- bug292: process_owned_page TOCTOU Ok(false)
- bug294: worker_mark_loop TOCTOU - 與此 bug 相同的模式
