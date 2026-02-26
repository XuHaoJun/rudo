# [Bug]: unified_write_barrier 缺少 has_gen_old_flag 快取導致 TOCTOU

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需精確時序才能觸發 |
| **Severity (嚴重程度)** | Medium | 導致 barrier 錯誤跳過，minor GC 可能錯誤回收年輕物件 |
| **Reproducibility (復現難度)** | High | 需極精確的執行時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `unified_write_barrier` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`unified_write_barrier` 函數在檢查 `has_gen_old_flag()` 時沒有像 `gc_cell_validate_and_barrier` 一樣快取結果，導致 TOCTOU (Time-of-Check-Time-of-Use) 漏洞。

### 預期行為 (Expected Behavior)
應該像 `gc_cell_validate_and_barrier` 一樣快取 `has_gen_old_flag()` 結果：
```rust
// Cache flag to avoid TOCTOU between check and barrier (bug114)
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
```

### 實際行為 (Actual Behavior)
直接內聯調用 `has_gen_old_flag()` 兩次（lines 2853 和 2876）：
```rust
if (*h_ptr).generation == 0 && !(*gc_box_addr).has_gen_old_flag() {
    return;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc_cell_validate_and_barrier` 中已經修復了相同的問題（bug114），但 `unified_write_barrier` 中遺漏了相同的修復。

問題在於：
1. 第一次調用 `has_gen_old_flag()` 讀取標記
2. 在 barrier 執行前，物件可能被 promoted（`gen_old_flag` 被設置）
3. 基於過時的 false 結果跳過 barrier

這會導致：
- OLD→YOUNG 引用沒有被記錄為 dirty
- Minor GC 可能錯誤回收年輕物件

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制才能可靠地重現：
1. 在 OLD 物件的 GcCell 上觸發 write barrier
2. 在 has_gen_old_flag() 檢查和 barrier 執行之間，物件被 promote
3. Young 物件被錯誤回收

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `unified_write_barrier` 的兩處位置快取 `has_gen_old_flag()` 結果：

```rust
// Large object path (line ~2853)
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // Add caching
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}

// Regular object path (line ~2876)
let gc_box_addr = (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // Add caching
if (*h.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
gen_old flag 的設計是為了 early-exit 優化，但必須正確處理 TOCTOU。否則會導致 barrier 失效。

**Rustacean (Soundness 觀點):**
未快取 flag 導致潛在的 data race，违反了 Rust 的内存模型。

**Geohot (Exploit 觀點):**
攻擊者可能利用此 TOCTOU 漏洞在極精確時序下導致 use-after-free。

---

## 驗證準則

- [ ] 確認 `unified_write_barrier` 中有兩處未快取的位置
- [ ] 檢查其他 barrier 函數是否有相同問題
- [ ] 確認修復後行為與 `gc_cell_validate_and_barrier` 一致
