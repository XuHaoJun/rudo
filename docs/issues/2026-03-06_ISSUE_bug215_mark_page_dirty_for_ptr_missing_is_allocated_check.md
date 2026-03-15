# [Bug]: mark_page_dirty_for_ptr 缺少 is_allocated 檢查 - 與 bug200/213 不同的code path

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 container tracing 並發執行，物件槽位被重用 |
| **Severity (嚴重程度)** | Medium | 導致不必要的頁面被加入 dirty_pages，影響 GC 效能 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_page_dirty_for_ptr` (heap.rs:3330)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`mark_page_dirty_for_ptr` 函數在將頁面加入 dirty_pages 列表時，沒有檢查該頁面中的物件槽位是否仍然被分配。這與 Bug 200/211/212/213 類似，但是發生在不同的 code path。

**Bug 200/211/212/213 涵蓋:**
- `unified_write_barrier` (heap.rs) - 檢查特定 object slot 的 is_allocated
- `GcThreadSafeCell::generational_write_barrier` - 小型/大型物件路徑 (cell.rs)
- `gc_cell_validate_and_barrier` (cell.rs)
- `simple_write_barrier` (cell.rs)

**本 Bug 涵蓋:**
- `mark_page_dirty_for_ptr` (heap.rs:3330) - 在頁面層級添加 dirty 追蹤

### 預期行為 (Expected Behavior)
在將頁面加入 dirty_pages 之前，應該檢查該頁面中是否有仍然被分配的物件。如果所有物件都已被 sweep，則不應該將頁面加入 dirty_pages。

### 實際行為 (Actual Behavior)
`mark_page_dirty_for_ptr` 直接調用 `heap.add_to_dirty_pages(header)` 沒有任何 is_allocated 檢查：

**heap.rs:3344-3345** - 大型物件路徑:
```rust
let header = unsafe { NonNull::new_unchecked(head_addr as *mut PageHeader) };
unsafe { heap.add_to_dirty_pages(header) };  // 沒有 is_allocated 檢查！
```

**heap.rs:3351-3352** - 小型物件路徑:
```rust
let header = unsafe { ptr_to_page_header(ptr) };
unsafe { heap.add_to_dirty_pages(header) };  // 沒有 is_allocated 檢查！
```

**heap.rs:3364-3365** - 跨執行緒大型物件路徑:
```rust
let header = unsafe { NonNull::new_unchecked(head_addr as *mut PageHeader) };
unsafe { heap.add_to_dirty_pages(header) };  // 沒有 is_allocated 檢查！
```

### 與 Bug 200/213 的差異

Bug 200/213 是針對特定 object slot (index) 的 dirty flag 設置：
```rust
(*header.as_ptr()).set_dirty(index);  // 修改 per-slot flag
```

本 Bug 是將整個頁面加入 dirty_pages 列表：
```rust
heap.add_to_dirty_pages(header);  // 添加整個頁面
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 container tracing 並發執行時：
1. 容器（例如 Vec）的 storage buffer 所在頁面中，某些 slot 被 sweep（釋放）
2. 容器被 trace（例如 `Vec<Gc<T>>` 的 trace）
3. `mark_page_dirty_for_ptr` 被調用以標記該頁面為 dirty
4. **BUG:** 頁面被添加到 dirty_pages，即使頁面中的物件可能已被釋放
5. 影響：GC 會不必要地掃描這些頁面，影響效能

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `mark_page_dirty_for_ptr` 的三個路徑中添加 is_allocated 檢查：

```rust
// 大型物件路徑
if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
    if ptr_addr >= head_addr + h_size && ptr_addr < head_addr + h_size + size {
        let header = unsafe { NonNull::new_unchecked(head_addr as *mut PageHeader) };
        // 檢查頁面是否仍然有分配物件
        unsafe {
            if (*header.as_ptr()).has_allocated_objects() {
                heap.add_to_dirty_pages(header);
            }
        }
    }
    return;
}
```

注意：需要實現 `PageHeader::has_allocated_objects()` 方法來檢查頁面中是否有任何已分配的物件。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 與 Bug 200/213 類似，但發生在不同的層級（頁面層級 vs 物件槽位層級）。Dirty page tracking 對於 incremental GC 的正確性很重要，但影響相對較小 - 主要是效能問題，不會導致記憶體損壞。

**Rustacean (Soundness 觀點):**
這不會導致 soundness 問題，因為只是將頁面添加到dirty列表。但可能導致 use-after-free 如果該頁面被完全釋放並重新分配。

**Geohot (Exploit 觀點):**
攻擊者可以嘗試控制 slot 重用的時序，來操縱 dirty_pages 的內容。但相對於 per-slot dirty flag 錯誤，這個 bug 的攻擊面較小。
