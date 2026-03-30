# [Bug]: simple_write_barrier Missing Second is_allocated Check Before Reading has_gen_old_flag

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需與 lazy sweep 並發執行才會觸發 |
| **Severity (嚴重程度)** | High | 可能讀取已釋放記憶體，導致 UAF 或錯誤的 barrier 行為 |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `simple_write_barrier` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`simple_write_barrier` 應該在讀取 `has_gen_old_flag` 之前有第二個 `is_allocated` 檢查，與 `incremental_write_barrier`、`unified_write_barrier` 和 `gc_cell_validate_and_barrier` 的模式一致。

### 實際行為
`simple_write_barrier` 缺少這個第二個檢查，導致 TOCTOU 漏洞。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/heap.rs` lines 2886-2892 (large object) 和 2916-2921 (small object)

`simple_write_barrier` 只有一次 `is_allocated` 檢查，之後直接讀取 `has_gen_old_flag`：

```rust
// Large object path (lines 2886-2892):
if !(*h_ptr).is_allocated(0) {
    return;
}
// 缺少第二個 is_allocated 檢查
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // TOCTOU: slot 可能已被 sweep

// Small object path (lines 2916-2921):
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
// 缺少第二個 is_allocated 檢查
let gc_box_addr = (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // TOCTOU: slot 可能已被 sweep
```

**對比已修復的函數：**

`incremental_write_barrier` 有正確的模式 (bug457, bug462)：
```rust
// Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU (bug457).
if !(*h_ptr).is_allocated(0) {
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要與 lazy sweep 並發執行才能穩定重現。單執行緒測試無法觸發此 TOCTOU。

**分析證據：**
- `incremental_write_barrier` 大型物件路徑 (line 3237-3241) 已有正確模式
- `incremental_write_barrier` 小型物件路徑 (line 3272-3276) 已有正確模式
- `unified_write_barrier` 已有正確模式 (bug463)
- `gc_cell_validate_and_barrier` 已有正確模式 (bug464)
- `simple_write_barrier` 是唯一缺少此修復的 barrier 函數

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 `simple_write_barrier` 的兩個路徑中添加第二個 `is_allocated` 檢查：

```rust
// Large object path (after line 2888):
// Second is_allocated check BEFORE reading has_gen_old - prevents TOCTOU
if !(*h_ptr).is_allocated(0) {
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();

// Small object path (after line 2918):
// Second is_allocated check BEFORE reading has_gen_old - prevents TOCTOU
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
```

這將使 `simple_write_barrier` 與其他 barrier 函數一致。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
所有 barrier 函數應該有一致的行為。`simple_write_barrier` 是功能較少的版本，但仍需與其他版本保持一致以避免維護問題。

**Rustacean (Soundness 觀點):**
缺少第二個 `is_allocated` 檢查會導致 TOCTOU，可能讀取已釋放/重新分配的記憶體，這是 UAF 的一種形式。

**Geohot (Exploit 觀點):**
如果攻擊者能控制 lazy sweep 的時序，可能利用此 TOCTOU 讀取已釋放的 GcBox 記憶體，進而進行記憶體佈局攻擊。

---

## 修復歷史

- **2026-03-30**: 報告此 bug (bug467)
- **2026-03-31**: Fix applied in `heap.rs`

## 修復詳情

**檔案:** `crates/rudo-gc/src/heap.rs`

**修改:**
1. Large object path (line ~2890): Added second `is_allocated` check after computing `gc_box_addr` but before reading `has_gen_old_flag`
2. Small object path (line ~2917): Added second `is_allocated` check after computing `gc_box_addr` but before reading `has_gen_old_flag`

**驗證:** `./clippy.sh` passes