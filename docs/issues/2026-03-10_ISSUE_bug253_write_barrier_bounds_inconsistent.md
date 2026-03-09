# [Bug]: Write Barrier 函數使用 `>` 進行 heap bounds 檢查，與 `is_in_range` 不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 pointer 精確落在 max_addr 位址，實際發生機率極低 |
| **Severity (嚴重程度)** | Low | 會導致不正確的 barrier 行為，但不太可能造成記憶體錯誤 |
| **Reproducibility (復現難度)** | Very High | 需要精確控制記憶體配置位址 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Write barrier functions (`simple_write_barrier`, `gc_cell_validate_and_barrier`, `unified_write_barrier`, `incremental_write_barrier`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
Write barrier 函數應該使用與 `LocalHeap::is_in_range` 一致的 bounds 檢查邏輯，確保任何被認為「在 heap 範圍內」的指標都會被 barrier 處理。

### 實際行為
多個 write barrier 函數使用 `ptr_addr > heap.max_addr` 進行檢查，但 `is_in_range` 使用 `addr < self.max_addr`（兩者不一致）：

- `simple_write_barrier` (heap.rs:2714): `if ptr_addr < heap.min_addr || ptr_addr > heap.max_addr`
- `gc_cell_validate_and_barrier` (heap.rs:2792): `if ptr_addr < heap.min_addr || ptr_addr > heap.max_addr`
- `unified_write_barrier` (heap.rs:2923): `if ptr_addr < heap.min_addr || ptr_addr > heap.max_addr`
- `incremental_write_barrier` (heap.rs:3008): `if ptr_addr < heap.min_addr || ptr_addr > heap.max_addr`

而 `is_in_range` (heap.rs:1782) 使用：
```rust
addr >= self.min_addr && addr < self.max_addr
```

### 不一致分析
假設 heap 配置在位址 [0x1000, 0x2000)（min_addr=0x1000, max_addr=0x2000）：
- `is_in_range(0x1FFF)` → true（最後一個有效位元組）
- Barrier 檢查 `ptr_addr > 0x2000`：0x1FFF > 0x2000 為 false，不會跳過 barrier（正確）

但對於 ptr_addr = 0x2000：
- `is_in_range(0x2000)` → false（超出 heap 範圍）
- Barrier 檢查：0x2000 > 0x2000 為 false，**不會跳過 barrier**（不正確！）

---

## 🔬 根本原因分析 (Root Cause Analysis)

`max_addr` 的語意是 exclusive upper bound（由 `self.max_addr = addr + size` 設定，見 heap.rs:1774）。

正確的檢查應該使用 `>=`：
```rust
if ptr_addr < heap.min_addr || ptr_addr >= heap.max_addr {
    return;
}
```

這與 `is_in_range` 的 `addr < self.max_addr` 邏輯一致。

---

## 💣 重現步驟 / 概念驗證 (PoC)

此 bug 需要精確控制記憶體配置，是理論層面的不一致性問題，實際觸發困難。

```rust
// 理論分析：
// 假設 GC heap 配置在 [0x1000, 0x2000)
// - min_addr = 0x1000
// - max_addr = 0x2000
//
// 當 ptr_addr = 0x2000 時：
// - is_in_range(0x2000) = false（正確：超出範圍）
// - 但 barrier 不會跳過：0x2000 > 0x2000 = false
// - 導致對無效指標執行 barrier 邏輯
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

將所有 barrier 函數中的 `ptr_addr > heap.max_addr` 改為 `ptr_addr >= heap.max_addr`：

```rust
// heap.rs:2714, 2792, 2923, 3008
if ptr_addr < heap.min_addr || ptr_addr >= heap.max_addr {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個理論層面的邊界條件問題。雖然實際觸發需要精確控制記憶體配置，但程式碼的一致性對於長期維護很重要。建議統一使用 `>=` 與 `is_in_range` 保持一致。

**Rustacean (Soundness 觀點):**
這不會造成 UB，因為後續的 MAGIC 檢查會捕获無效指標。但可能導致輕微的效能開銷和程式碼理解困難。

**Geohot (Exploit 觀點):**
實際利用此 bug 的機會極低，因為需要精確控制記憶體配置位址。但理論上，如果攻擊者能控制配置位址，可能利用此不一致性繞過某些檢查。
