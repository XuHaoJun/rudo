# [Bug]: find_gc_box_from_ptr 大型物件路徑缺少 MAGIC 驗證

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Rare | large_object_map 包含無效條目的情況極為罕見 |
| **Severity (嚴重程度)** | Medium | 可能導致讀取無效記憶體或錯誤的 GC 追蹤行為 |
| **Reproducibility (復現難度)** | Very High | 需要人為注入無效的 large_object_map 條目 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `find_gc_box_from_ptr` (heap.rs:3452-3573)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`find_gc_box_from_ptr` 應該在所有路徑中驗證 `MAGIC_GC_PAGE` 魔數，以確保記憶體安全。

### 實際行為 (Actual Behavior)
- **大型物件路徑** (lines 3479-3493)：從 `large_object_map` 獲取頁面位址後，直接使用 header 指針，**未驗證**魔數
- **小型物件路徑** (line 3518)：正確驗證 `(*header_ptr).magic == MAGIC_GC_PAGE`

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs:3452-3573` 的 `find_gc_box_from_ptr` 函數中：

```rust
// 大型物件路徑 - 缺少 MAGIC 驗證 (BUG)
if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    let h_ptr = head_addr as *mut PageHeader;
    // 直接使用 h_ptr，無魔數檢查！
    if addr < head_addr + h_size {
        return None;
    }
    (h_ptr, size, h_size, addr - (head_addr + h_size))
} else {
    // 小型物件路徑 - 有魔數驗證 (正確)
    let mut header_ptr = ptr_to_page_header(ptr).as_ptr();
    // ...
    if (*header_ptr).magic == MAGIC_GC_PAGE {  // <-- 這個檢查在大型物件路徑中缺失
        // ...
    }
}
```

**問題**：大型物件路徑假設 `large_object_map` 中的條目永遠有效，但這違反了防御性編程原則。

**此函數與 bug190 的關係**：
- bug190 涵蓋 `gc_cell_validate_and_barrier` 和 `unified_write_barrier`
- 本 bug 涵蓋 `find_gc_box_from_ptr` - 這是第三個有同樣問題的函數

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 難以自然觸發，需要人為構造無效的 `large_object_map` 條目：

```rust
// 概念驗證（需要 internal API 或 unsafe）
fn main() {
    // 需要一種方式在 large_object_map 中注入無效條目
    // 然後呼叫 find_gc_box_from_ptr
    // 如果條目無效，程式可能讀取垃圾值
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在大型物件路徑中添加魔數驗證：

```rust
if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    let h_ptr = head_addr as *mut PageHeader;
    
    // 添加魔數驗證（與小型物件路徑一致）
    if (*h_ptr).magic != MAGIC_GC_PAGE {
        return None;
    }
    
    if addr < head_addr + h_size {
        return None;
    }
    (h_ptr, size, h_size, addr - (head_addr + h_size))
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是防御性編程的基本原則。即使 `large_object_map` 目前沒有已知的腐敗途徑，所有記憶體訪問都應該經過驗證。這個函數是 GC 的關鍵路徑，任何未定義行為都可能導致嚴重後果。

**Rustacean (Soundness 觀點):**
缺少驗證違反了防御性編程原則。如果未來某處存在 bug 導致 `large_object_map` 包含無效條目，此處可能成為未定義行為的觸發點。與 bug190 相同的問題模式再次出現，表明需要全面審查。

**Geohot (Exploit 觀點):**
此問題難以直接利用，因為需要先破壞 `large_object_map`。但如果攻擊者能夠控制該映射（例如透過其他記憶體損壞漏洞），則可能利用此處缺乏驗證來進一步擴大攻擊面。

---

## 驗證記錄 (Verification Record)

**驗證日期:** 2026-03-05
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `crates/rudo-gc/src/heap.rs` 的 `find_gc_box_from_ptr` 函數：

1. 大型物件路徑 (lines 3479-3493)：從 `large_object_map.get(&page_addr)` 獲取 header 後直接使用，無 MAGIC 檢查
2. 小型物件路徑 (line 3518)：有 `(*header_ptr).magic == MAGIC_GC_PAGE` 檢查
3. 這與 bug190 描述的問題模式相同，但發生在不同的函數中

**結論:** Bug 確認存在，需要修復以確保所有路徑都驗證 MAGIC。
