# [Bug]: simple_write_barrier 缺少 MAGIC_GC_PAGE 驗證導致潛在 UB

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 large_object_map 包含無效条目，屬於內部一致性问题 |
| **Severity (嚴重程度)** | Medium | 可能讀取無效記憶體，但實際觸發條件較嚴苛 |
| **Reproducibility (復現難度)** | Medium | 程式碼審查可確認，無效 map 条目理論上不會發生 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `simple_write_barrier` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`simple_write_barrier` 函數處理大物件時應該驗證 `MAGIC_GC_PAGE`，確保 `large_object_map` 中的条目有效。這個驗證存在於其他類似的 barrier 函數中。

### 實際行為 (Actual Behavior)

`simple_write_barrier` 函數在處理大物件時**缺少** `MAGIC_GC_PAGE` 驗證，而其他類似的 barrier 函數都有這個驗證。

---

## 🔬 根本原因分析 (Root Cause Analysis)

對比程式碼：

**`simple_write_barrier` (lines 2793-2809) - 缺少 MAGIC 驗證：**
```rust
let h_ptr = head_addr as *mut PageHeader;
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// 缺少：if (*h_ptr).magic != MAGIC_GC_PAGE { return; }
```

**`gc_cell_validate_and_barrier` (lines 2887-2891) - 有 MAGIC 驗證：**
```rust
let h_ptr = head_addr as *mut PageHeader;

// Validate MAGIC to ensure the large_object_map entry is valid (bug190).
if (*h_ptr).magic != MAGIC_GC_PAGE {
    return;
}
```

**`unified_write_barrier` (lines 3018-3022) - 有 MAGIC 驗證：**
```rust
let h_ptr = head_addr as *mut PageHeader;

// Validate MAGIC to ensure the large_object_map entry is valid (bug190).
if (*h_ptr).magic != MAGIC_GC_PAGE {
    return;
}
```

**`incremental_write_barrier` (lines 3114-3117) - 有 MAGIC 驗證：**
```rust
let h_ptr = head_addr as *mut PageHeader;
// Validate MAGIC to ensure the large_object_map entry is valid (bug190).
if (*h_ptr).magic != MAGIC_GC_PAGE {
    return;
}
```

缺少此驗證可能導致：
1. 讀取無效的 PageHeader 欄位
2. 對已釋放或未初始化的記憶體進行操作

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此問題屬於程式碼一致性问题。理論上 `large_object_map` 不會包含無效条目（因為 map 只在配置大物件時寫入），但為保持一致性和防御性，應該添加驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `simple_write_barrier` 的大物件處理分支中添加 MAGIC 驗證：

```rust
let h_ptr = head_addr as *mut PageHeader;

// Validate MAGIC to ensure the large_object_map entry is valid.
if (*h_ptr).magic != MAGIC_GC_PAGE {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 程式碼一致性對於維護非常重要
- 其他 barrier 都有這個驗證，顯示這是必要的防御措施
- 缺少驗證可能導致在異常情況下讀取無效記憶體

**Rustacean (Soundness 觀點):**
- 雖然理論上 map 條目總是有效的，但防御性編碼是 Rust 的最佳實踐
- 缺少驗證可能在未來維護中造成問題

**Geohot (Exploit 觀點):**
- 如果有辦法在 large_object_map 中注入無效条目，這可能是一個攻擊面
- 攻擊難度較高，但存在理論上的可能性

---

## Resolution (2026-03-21)

**Fixed:** In `crates/rudo-gc/src/heap.rs`, `simple_write_barrier`’s large-object branch now validates `(*h_ptr).magic == MAGIC_GC_PAGE` before any further `PageHeader` / `GcBox` reads, matching `gc_cell_validate_and_barrier`, `unified_write_barrier`, and `incremental_write_barrier` (bug190 pattern).

**Verification:** `./clippy.sh` and `./test.sh` passed. `simple_write_barrier` remains `#[allow(dead_code)]`; change is defensive consistency only.
