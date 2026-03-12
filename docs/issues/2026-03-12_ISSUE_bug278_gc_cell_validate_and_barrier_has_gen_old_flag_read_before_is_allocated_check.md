# [Bug]: gc_cell_validate_and_barrier has_gen_old_flag 讀取在 is_allocated 檢查之前 - TOCTOU

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發環境：lazy sweep 與 mutator 同時執行 |
| **Severity (嚴重程度)** | High | 可能導致 barrier 讀取已釋放物件的 flag，dirty tracking 混亂 |
| **Reproducibility (復現難度)** | High | 需要精確時序控制：slot sweep → reuse → flag read → is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc_cell_validate_and_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
應該先檢查 slot 是否仍然被分配 (`is_allocated()`)，然後再從該 slot 讀取任何 flag。

### 實際行為
在 `gc_cell_validate_and_barrier` 函數中：
1. **Line 2886**: 從 slot 讀取 `has_gen_old_flag()`
2. **Line 2894**: 檢查 `is_allocated(index)`

這導致在 lazy sweep 回收並重用 slot 後，barrier 可能讀取到已釋放物件的 stale flag。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs` 的 `gc_cell_validate_and_barrier` 函數：

```rust
// Line 2883-2889: 先讀取 flag
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // <-- 先讀取
if (*h).generation == 0 && !has_gen_old {
    return;
}

// Line 2893-2896: 後檢查 is_allocated
// Skip if slot was swept; avoids corrupting dirty tracking with reused slot (bug211).
if !(*h.as_ptr()).is_allocated(index) {  // <-- 後檢查
    return;
}
```

並發場景：
1. Mutator A 正在執行 write barrier，計算得到 slot `index`
2. Mutator A 從 slot 讀取 `has_gen_old_flag()` (此時 slot 包含物件 A)
3. GC 執行 lazy sweep，回收物件 A 並將 slot 分配給新物件 B
4. Mutator A 檢查 `is_allocated(index)` - 通過（因為 slot 現在分配給 B）
5. Mutator A 使用從舊物件 A 讀取的 flag 值執行 barrier

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 創建包含 GcCell 的 GC 物件
2. 在多個執行緒中並發修改 GcCell
3. 同時觸發 GC 進行 lazy sweep
4. 觀察 barrier 行為是否異常

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `is_allocated` 檢查移到 `has_gen_old_flag` 讀取之前：

```rust
// 先檢查 is_allocated
if !(*h.as_ptr()).is_allocated(index) {
    return;
}

// 後讀取 flag
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h).generation == 0 && !has_gen_old {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
讀取已釋放物件的 flag 會導致 barrier 做出錯誤的決定。這可能導致 OLD→YOUNG 引用被錯誤地忽略，導致 young 物件被錯誤回收。

**Rustacean (Soundness 觀點):**
這是經典的 TOCTOU 漏洞。雖然不會直接導致 UAF，但會導致不一致的 barrier 行為，可能導致記憶體錯誤。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個時序漏洞來控制 barrier 行為，進一步利用記憶體佈局進行攻擊。
