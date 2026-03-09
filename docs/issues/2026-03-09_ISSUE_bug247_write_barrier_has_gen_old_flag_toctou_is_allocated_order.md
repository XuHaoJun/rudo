# [Bug]: Write barrier TOCTOU - has_gen_old_flag read BEFORE is_allocated check

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發環境：lazy sweep 與 mutator 同時執行 |
| **Severity (嚴重程度)** | High | 可能導致 barrier 記錄無效的 slot，dirty tracking 混亂 |
| **Reproducibility (重現難度)** | High | 需要精確時序控制：slot sweep → reuse → flag read → is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `generational_write_barrier`, `unified_write_barrier`, `gc_cell_validate_and_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

在 `heap.rs` 的多個 write barrier 函數中，`has_gen_old_flag()` 在 `is_allocated()` 檢查之前被讀取。這創造了一個 TOCTOU (Time-Of-Check-Time-Of-Use) 競爭條件：

### 受影響的函數：
1. `generational_write_barrier` (lines 2727-2768)
2. `unified_write_barrier` (lines 2944-2989)
3. `gc_cell_validate_and_barrier` (lines 2817-2861)
4. `incremental_write_barrier` (lines 3001-3049)

### 預期行為
應該先檢查 slot 是否仍然被分配 (`is_allocated()`)，然後再從該 slot 讀取任何 flag。

### 實際行為
當前順序：
1. 從 slot 讀取 `has_gen_old_flag()` 
2. 檢查 `is_allocated(index)`

這導致在 lazy sweep 回收並重用 slot 後，barrier 可能讀取到已釋放物件的 stale flag。

---

## 🔬 根本原因分析 (Root Cause Analysis)

並發場景：
1. Mutator A 正在執行 write barrier，計算得到 slot `index`
2. Mutator A 從 slot 讀取 `has_gen_old_flag()` (此時 slot 包含物件 A)
3. Lazy sweep 執行，回收 slot 並分配給新物件 B
4. Mutator A 檢查 `is_allocated(index)` → 通過 (slot 現為物件 B)
5. Mutator A 使用來自舊物件 A 的 stale flag 做出 barrier 決策

這與 bug200/bug212/bug220 不同：
- 那些 bug 是關於「完全缺少」is_allocated 檢查
- 此 bug 是關於「順序錯誤」- is_allocated 檢查在 flag 讀取之後

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟用 lazy sweep
2. Thread A：不斷分配/釋放物件，重用相同 slot
3. Thread B：不斷觸發 write barrier 到相同 slot
4. 觀察 barrier 行為是否異常

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在每個受影響的函數中，將 `is_allocated` 檢查移到 `has_gen_old_flag()` 讀取之前：

```rust
// 錯誤的順序（當前）：
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // 讀取 flag
if (*h.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
if !(*header.as_ptr()).is_allocated(index) {  // 檢查分配
    return;
}

// 正確的順序：
if !(*header.as_ptr()).is_allocated(index) {  // 先檢查分配
    return;
}
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // 再讀取 flag
if (*h.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 與之前修復的 bug (bug114, bug133, bug144, bug149) 不同。之前的 bug 是關於「緩存 flag 以避免在兩次檢查之間變化」，此 bug 是關於「在讀取 flag 之前必須確保 slot 仍然有效」。這是記憶體安全的基礎原則。

**Rustacean (Soundness 觀點):**
從已釋放的記憶體讀取 flag 雖然不會直接導致 UAF（因為記憶體可能還沒被覆蓋），但讀取 stale 資料並據此做決策可能導致不一致的 barrier 行為，進而影響 GC 正確性。

**Geohot (Exploit 觀點):**
在極端情況下，攻擊者可能控制時序，使 barrier 記錄無效的 slot 到 dirty_pages 或 remembered_set，導致後續掃描發生異常。
