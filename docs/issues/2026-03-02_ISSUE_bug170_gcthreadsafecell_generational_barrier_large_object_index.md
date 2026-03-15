# [Bug]: GcThreadSafeCell generational_write_barrier 對大型物件的 index 計算可能錯誤

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 只在 GcThreadSafeCell 持有大型物件時觸發 |
| **Severity (嚴重程度)** | High | 可能導致錯誤的 dirty page 追蹤，影響世代 GC 正確性 |
| **Reproducibility (復現難度)** | Medium | 需要使用 GcThreadSafeCell 包裝大型物件並進行 mutation |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::generational_write_barrier` (cell.rs:1190-1243)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

在 `GcThreadSafeCell::generational_write_barrier` 函數中，處理大型物件時的邏輯與 `unified_write_barrier` (heap.rs:2881-2937) 不一致。

### 預期行為 (Expected Behavior)
對於大型物件，應該總是使用 index 0 來標記 dirty bit，如同 `unified_write_barrier` 的做法。

### 實際行為 (Actual Behavior)
`GcThreadSafeCell::generational_write_barrier` 對大型物件計算 index：`index = offset / block_size`

這可能會對大型物件計算出非 0 的 index，導致標記錯誤的 slot 為 dirty。

### 程式碼位置
- **有問題的程式碼**: `cell.rs:1200-1219`
- **正確的參考**: `heap.rs:2895-2907`

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `GcThreadSafeCell::generational_write_barrier` 中 (cell.rs:1200-1219):

```rust
if is_large {
    if let Some(&(head_addr, _, _)) = heap.large_object_map.get(&page_addr) {
        let header = head_addr as *mut crate::heap::PageHeader;
        if (*header).magic == MAGIC_GC_PAGE && (*header).generation > 0 {
            let block_size = (*header).block_size as usize;
            let header_size = (*header).header_size as usize;
            let header_page_addr = head_addr;
            let ptr_addr = ptr as usize;

            if ptr_addr >= header_page_addr + header_size {
                let offset = ptr_addr - (header_page_addr + header_size);
                let index = offset / block_size;  // <-- 問題：計算 index

                if index < (*header).obj_count as usize {
                    (*header).set_dirty(index);
                    heap.add_to_dirty_pages(NonNull::new_unchecked(header));
                }
            }
        }
    }
}
```

但在 `unified_write_barrier` (heap.rs:2895-2907) 中:

```rust
let (header, index) =
    if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
        // ...
        (NonNull::new_unchecked(h_ptr), 0_usize)  // <-- 正確：總是使用 index 0
    }
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 創建一個大型物件 (> page_size)
2. 使用 `GcThreadSafeCell` 包裝該大型物件
3. 將物件 promotion 到 old generation
4. 調用 `borrow_mut()` 進行 mutation
5. 檢查 dirty page 追蹤是否正確

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `GcThreadSafeCell::generational_write_barrier` 中的大型物件處理邏輯，使其與 `unified_write_barrier` 一致，總是使用 index 0：

```rust
if is_large {
    if let Some(&(head_addr, _, _)) = heap.large_object_map.get(&page_addr) {
        let header = head_addr as *mut crate::heap::PageHeader;
        if (*header).magic == MAGIC_GC_PAGE && (*header).generation > 0 {
            // 對於大型物件，總是使用 index 0
            (*header).set_dirty(0);
            heap.add_to_dirty_pages(NonNull::new_unchecked(header));
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 大型物件在分頁配置中通常佔用整個頁面（或多頁面）
- 對於大型物件，概念上只有一個 object，index 應該總是 0
- 這與 unified_write_barrier 的處理方式一致

**Rustacean (Soundness 觀點):**
- 錯誤的 index 可能導致標記錯誤的記憶體區域為 dirty
- 這不會導致 UAF，但可能導致 GC 正確性問題

**Geohot (Exploit 觀點):**
- 這是一個邏輯錯誤，可能被利用來影響 GC 時序
- 但不太可能直接導致安全漏洞

---

## Resolution (2026-03-03)

**Outcome:** Fixed.

`GcThreadSafeCell::generational_write_barrier` was computing `index = offset / block_size` for large objects, which could produce non-zero indices and mark the wrong slot. The fix aligns with `unified_write_barrier` (heap.rs): for large objects, always use `index 0`. Also added the bounds check `ptr_addr < head_addr + h_size || ptr_addr >= head_addr + h_size + size` for consistency.
