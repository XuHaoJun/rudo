# [Bug]: incremental_write_barrier 缺少 is_allocated 檢查與大物件處理

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 函數標記為 #[allow(dead_code)]，目前未被使用，但為潛在缺陷 |
| **Severity (嚴重程度)** | High | 可能導致 dirty tracking 混亂，影響 GC 正確性 |
| **Reproducibility (重現難度)** | High | 需要啟用函數並精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `incremental_write_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`incremental_write_barrier` 函數有 **兩處問題**，與 Bug 200/211/212 類似但發生在不同 code path：

### 已文檔化的 Bug 涵蓋範圍：
- **Bug 200:** `unified_write_barrier` 缺少 is_allocated 檢查
- **Bug 211:** `gc_cell_validate_and_barrier` 缺少 is_allocated 檢查
- **Bug 212:** `simple_write_barrier` 缺少 is_allocated 檢查

### 本 Bug 涵蓋：
`incremental_write_barrier` (heap.rs) - 標記為 `#[allow(dead_code)]`

### 預期行為 (Expected Behavior)
1. 在設置 dirty flag 或訪問物件前，應該先檢查該 slot 是否仍然被分配
2. 應該正確處理大物件 (large object)，檢查 `heap.large_object_map`

### 實際行為 (Actual Behavior)

**問題 1: 缺少 is_allocated 檢查 (heap.rs:3014-3023)**
```rust
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // 沒有 is_allocated 檢查！
if (*header.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
heap.record_in_remembered_buffer(header);
```

**問題 2: 缺少大物件處理**
不同於 `simple_write_barrier` (line 2712 有 large_object_map 檢查)，`incremental_write_barrier` 完全没有檢查 `heap.large_object_map`，直接調用 `ptr_to_page_header(ptr)`。

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
4. `incremental_write_barrier` 計算相同的 `index`
5. **BUG 1:** 對已釋放的 slot 調用 `has_gen_old_flag()` - 可能訪問無效記憶體
6. **BUG 2:** 大物件的情況下 `ptr_to_page_header()` 會返回無效的 header

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟用 `incremental_write_barrier` 函數（移除 `#[allow(dead_code)]`）
2. 啟用 lazy sweep feature
3. 一個執行緒不斷分配/釋放物件重用槽位
4. 另一個執行緒不斷觸發 incremental write barrier
5. 觀察是否發生記憶體錯誤

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 添加 `is_allocated` 檢查：
```rust
// 檢查 slot 是否仍然被分配
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
```

2. 添加大物件處理：
```rust
let (header, index) = if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    // Large object path...
} else {
    // Small object path...
};
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 與 Bug 200/211/212 類似，但發生在不同的 code path (`incremental_write_barrier`)。此外還缺少大物件處理，這在其他三個 barrier 函數中都有實現。

**Rustacean (Soundness 觀點):**
缺少 is_allocated 檢查可能導致 access to freed memory。缺少大物件處理可能導致读取错误的 header 数据。

**Geohot (Exploit 觀點):**
雖然函數目前標記為 dead code，但若未來啟用，可能成為攻擊向量。

---

## Resolution (2026-03-14)

**Fixed.** The `is_allocated` check was already present (bug286). The missing piece was **large object handling**: `incremental_write_barrier` now checks `heap.large_object_map` before calling `ptr_to_page_header`, matching the pattern used in `simple_write_barrier` and `gc_cell_validate_and_barrier`. For tail pages of multi-page large objects, `ptr_to_page_header` would have yielded garbage; the fix routes through `large_object_map` to obtain the correct header.
