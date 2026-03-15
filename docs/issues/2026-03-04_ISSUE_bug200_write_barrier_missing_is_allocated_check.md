# [Bug]: Write Barrier 缺少 is_allocated 檢查 - 可能標記錯誤的物件槽位

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 mutator 並發執行，物件槽位被重用 |
| **Severity (嚴重程度)** | High | 可能導致 dirty tracking 混亂，影響 GC 正確性 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Write Barrier (`unified_write_barrier`, `GcThreadSafeCell::generational_write_barrier`)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

在 write barrier 的實作中，計算物件 index 後直接調用 `set_dirty(index)`，但沒有檢查該 index 是否仍然被分配。這與 Bug 78 類似，但是發生在不同的 code path。

### 預期行為 (Expected Behavior)
在設置 dirty flag 之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該調用 `set_dirty`。

### 實際行為 (Actual Behavior)
兩處函數都缺少 `is_allocated` 檢查：

1. `heap.rs:2944` - `unified_write_barrier`:
```rust
let index = offset / block_size;
// ... 檢查 obj_count 和 generation ...
// 沒有 is_allocated 檢查！
(*header.as_ptr()).set_dirty(index);
heap.add_to_dirty_pages(header);
```

2. `cell.rs:1224` - `GcThreadSafeCell::generational_write_barrier`:
```rust
if index < (*header.as_ptr()).obj_count as usize {
    // 沒有 is_allocated 檢查！
    (*header.as_ptr()).set_dirty(index);
    heap.add_to_dirty_pages(header);
}
```

對比：正確的實作在 `gc/incremental.rs:1007-1010`:
```rust
// Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
if !(*h).is_allocated(idx) {
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 mutator 並發執行時：
1. 物件 A 在 slot `index` 被 sweep（釋放）
2. 物件 B 在同一個 slot 被重新分配
3. Mutator 寫入物件 B 的欄位
4. Write barrier 計算相同的 `index`
5. **BUG:** 對物件 B 的 slot 調用 `set_dirty(index)` - 破壞 dirty tracking
6. 這可能導致不正確的 GC 行為或記憶體損壞

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟用 lazy sweep feature
2. 一個執行緒不斷分配/釋放物件重用槽位
3. 另一個執行緒不斷觸發 write barrier
4. 觀察 dirty_pages 是否包含無效的 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `unified_write_barrier` (heap.rs:2944) 添加 `is_allocated` 檢查：
```rust
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
(*header.as_ptr()).set_dirty(index);
```

在 `GcThreadSafeCell::generational_write_barrier` (cell.rs:1224) 添加同樣的檢查。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 與 Bug 78 類似，但發生在不同的 code path。Write barrier 的 dirty tracking 對於 incremental GC 的正確性至關重要。如果對已釋放的 slot 設置 dirty flag，可能導致：
- 頁面被錯誤地加入 dirty_pages
- 掃描時訪問無效記憶體
- 記憶體佈局混淆

**Rustacean (Soundness 觀點):**
這不是傳統意義的 UB，但可能導致 use-after-free 類型的問題。當 slot 被重用後，舊的 metadata（如 dirty flag）可能干擾新的物件。

**Geohot (Exploit 觀點):**
攻擊者可以嘗試控制 slot 重用的時序，來操縱 dirty_pages 的內容。這可能成為 exploit 的著手點。
