# [Bug]: simple_write_barrier TOCTOU - has_gen_old_flag called without caching

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 函數為 dead code，目前未被啟用 |
| **Severity (嚴重程度)** | High | TOCTOU 可能導致 OLD→YOUNG 引用遺漏，導致年輕物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需啟用函數並觸發特定時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap::simple_write_barrier`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為
`simple_write_barrier` 應該與 `unified_write_barrier` 一致，在檢查 `has_gen_old_flag` 和執行 barrier 操作之間快取 flag 值以避免 TOCTOU。

### 實際行為
`simple_write_barrier` 在兩處直接調用 `has_gen_old_flag()` 而非先快取：
- Line 2664: `if (*h_ptr).generation == 0 && !(*gc_box_addr).has_gen_old_flag()`
- Line 2687: `if (*h.as_ptr()).generation == 0 && !(*gc_box_addr).has_gen_old_flag()`

這與 `unified_write_barrier` 的修復模式不一致（bug133）。

---

## 🔬 根本原因分析 (Root Cause Analysis)

`unified_write_barrier` 在 bug133 中已修復 TOCTOU 問題：
```rust
// Line 2854: 快取 flag
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
```

但 `simple_write_barrier` 沒有套用相同修復。函數目前標記為 `#[allow(dead_code)]`，因此未被啟用，但這是潛在的代碼缺陷。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此函數目前為 dead code，無法直接觸發。若要觸發：
1. 移除 `#[allow(dead_code)]` 屬性
2. 在增量標記期間呼叫 `simple_write_barrier`
3. 在 has_gen_old_flag() 檢查和 barrier 執行之間觸發 GC 執行緒干預

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `unified_write_barrier` 的 TOCTOU 修復模式應用於 `simple_write_barrier`：

```rust
// 2664 行附近
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}

// 2687 行附近  
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`simple_write_barrier` 缺少 TOCTOU 修復，這與 `unified_write_barrier` 的設計不一致。在增量標記期間，如果物件的 gen_old_flag 在檢查後被清除，barrier 可能會被錯誤地跳過。

**Rustacean (Soundness 觀點):**
這不是直接的安全問題（函數未啟用），但代碼不一致性可能在未來啟用時導致記憶體回收錯誤。

**Geohot (Exploit 觀點):**
攻擊者無法直接利用 dead code，但代碼審計應確保所有 barrier 實作有一致的 TOCTOU 防護。
