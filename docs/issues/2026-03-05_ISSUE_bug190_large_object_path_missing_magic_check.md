# [Bug]: gc_cell_validate_and_barrier 與 unified_write_barrier 大型物件路徑缺少 MAGIC 驗證

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Rare | large_object_map 包含無效條目的情況極為罕見 |
| **Severity (嚴重程度)** | Medium | 可能導致 barrier 行為錯誤或讀取無效記憶體 |
| **Reproducibility (復現難度)** | Very High | 需要人為注入無效的 large_object_map 條目 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc_cell_validate_and_barrier`, `unified_write_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

在 `gc_cell_validate_and_barrier` 和 `unified_write_barrier` 函數中，大型物件路徑缺少 `MAGIC_GC_PAGE` 魔數驗證，而小型物件路徑正確地進行了驗證。

### 預期行為 (Expected Behavior)
所有程式碼路徑在存取 `PageHeader` 欄位前都應該驗證 `MAGIC_GC_PAGE`，以確保記憶體安全。

### 實際行為 (Actual Behavior)
- **大型物件路徑**：從 `large_object_map` 獲取頁面位址後，直接存取 `PageHeader` 欄位（`generation`、`owner_thread`），**未驗證**魔數
- **小型物件路徑**：正確驗證 `if (*h).magic != MAGIC_GC_PAGE { return; }`

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs:2786-2818` 的 `gc_cell_validate_and_barrier` 函數中：

```rust
// 大型物件路徑 - 缺少 MAGIC 驗證 (BUG)
if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    // ...
    let h_ptr = head_addr as *mut PageHeader;
    let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
    // 直接存取 header 欄位，無魔數檢查！
    let has_gen_old = (*gc_box_addr).has_gen_old_flag();
    if (*h_ptr).generation == 0 && !has_gen_old {
        return;
    }
    // ...
} else {
    // 小型物件路徑 - 有魔數驗證 (正確)
    let header = ptr_to_page_header(ptr);
    let h = header.as_ptr();
    if (*h).magic != MAGIC_GC_PAGE {  // <-- 這個檢查在大型物件路徑中缺失
        return;
    }
    // ...
}
```

同樣的問題也存在於 `unified_write_barrier` (heap.rs:2904-2942)。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 難以自然觸發，需要人為構造無效的 `large_object_map` 條目：

```rust
// 概念驗證（需要 internal API 或 unsafe）
fn main() {
    // 需要一種方式在 large_object_map 中注入無效條目
    // 然後呼叫 gc_cell_validate_and_barrier
    // 如果條目無效，程式可能讀取垃圾值或崩潰
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在大型物件路徑中添加魔數驗證：

```rust
if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + h_size + size {
        return;
    }
    let h_ptr = head_addr as *mut PageHeader;
    
    // 添加魔數驗證（與小型物件路徑一致）
    if (*h_ptr).magic != MAGIC_GC_PAGE {
        return;
    }
    
    let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 實現中，假設內部資料結構始終有效是危險的。即使 `large_object_map` 目前沒有已知的腐敗途徑，防御性編程要求對所有記憶體訪問進行驗證。這與小型物件路徑的一致性也很重要。

**Rustacean (Soundness 觀點):**
雖然此問題不太可能在正常操作中觸發，但缺少驗證違反了防御性編程原則。如果未來某處存在 bug 導致 `large_object_map` 包含無效條目，此處可能成為未定義行為的觸發點。

**Geohot (Exploit 觀點):**
此問題難以直接利用，因為需要先破壞 `large_object_map`。但如果攻擊者能夠控制該映射（例如透過其他記憶體損壞漏洞），則可能利用此處缺乏驗證來進一步擴大攻擊面。
