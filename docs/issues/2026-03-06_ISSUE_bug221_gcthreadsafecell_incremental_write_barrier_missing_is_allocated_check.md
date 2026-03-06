# [Bug]: GcThreadSafeCell::incremental_write_barrier 缺少 is_allocated 檢查與大物件處理

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 函數標記為 #[allow(dead_code)]，目前未被使用，但為潛在缺陷 |
| **Severity (嚴重程度)** | High | 可能導致 dirty tracking 混亂，影響 GC 正確性 |
| **Reproducibility (重現難度)** | High | 需要啟用函數並精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::incremental_write_barrier` (cell.rs:1150-1181)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`GcThreadSafeCell::incremental_write_barrier` 函數有 **兩處問題**，與 Bug 220 類似但發生在不同 code path：

### 已文檔化的 Bug 涵蓋範圍：
- **Bug 200:** `unified_write_barrier` 缺少 is_allocated 檢查
- **Bug 211:** `gc_cell_validate_and_barrier` 缺少 is_allocated 檢查
- **Bug 212:** `simple_write_barrier` 缺少 is_allocated 檢查
- **Bug 220:** `incremental_write_barrier` (heap.rs) 缺少 is_allocated 檢查

### 本 Bug 涵蓋：
`GcThreadSafeCell::incremental_write_barrier` (cell.rs) - 標記為 `#[allow(dead_code)]`

### 預期行為 (Expected Behavior)
1. 在訪問物件前，應該先檢查該 slot 是否仍然被分配
2. 應該正確處理大物件 (large object)，檢查 `heap.large_object_map`

### 實際行為 (Actual Behavior)

**問題 1: 缺少 is_allocated 檢查 (cell.rs:1172-1179)**
```rust
let header = ptr_to_page_header(ptr);
if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
    return;
}

if (*header.as_ptr()).generation > 0 {
    let _ = record_page_in_remembered_buffer(header);  // 沒有 is_allocated 檢查！
}
```

不同於 `mark_object_black` (gc/incremental.rs:1007-1010)，此函數沒有檢查 `is_allocated`：
```rust
// 正確的實作：
if !(*h).is_allocated(idx) {
    return None;
}
```

**問題 2: 缺少大物件處理**
不同於 `GcThreadSafeCell::generational_write_barrier` (有 large_object_map 檢查)，`incremental_write_barrier` 完全没有檢查 `heap.large_object_map`，直接調用 `ptr_to_page_header(ptr)`。

對比 `simple_write_barrier` (heap.rs:2712) 有正確的 large object 處理：
```rust
if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    // Large object path...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 mutator 並發執行時：
1. 物件 A 在某個 slot 被 sweep（釋放）
2. 物件 B 在同一個 slot 被重新分配
3. Mutator 寫入物件 B 的欄位
4. `GcThreadSafeCell::incremental_write_barrier` 計算相同的 slot
5. **BUG 1:** 對已釋放的 slot 調用 `generation` 檢查 - 可能訪問無效記憶體
6. **BUG 2:** 大物件的情況下 `ptr_to_page_header()` 會返回無效的 header

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟用 `GcThreadSafeCell::incremental_write_barrier` 函數（移除 `#[allow(dead_code)]`）
2. 啟用 lazy sweep feature
3. 一個執行緒不斷分配/釋放物件重用槽位
4. 另一個執行緒不斷觸發 incremental write barrier
5. 觀察是否發生記憶體錯誤

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 添加 `is_allocated` 檢查（小型物件路徑）：
```rust
let index = offset / block_size;
if index >= obj_count {
    return;
}
// 添加 is_allocated 檢查
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
```

2. 添加大物件處理：
```rust
let (header, index) = if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    // Large object path with is_allocated check...
} else {
    // Small object path with is_allocated check...
};
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 與 Bug 220 類似，但發生在 `GcThreadSafeCell` 的 code path 中。此外還缺少大物件處理。缺少這些檢查會導致 barrier 在 slot 被回收後繼續執行，可能導致錯誤的 dirty page 追蹤。

**Rustacean (Soundness 觀點):**
缺少 is_allocated 檢查可能導致 use-after-free。缺少大物件處理可能導致讀取錯誤的 header 數據。

**Geohot (Exploit 觀點):**
雖然函數目前標記為 dead code，但若未來啟用，可能成為攻擊向量。攻擊者可能透過觸發 slot 回收並重用來觸發不一致的 barrier 狀態。
