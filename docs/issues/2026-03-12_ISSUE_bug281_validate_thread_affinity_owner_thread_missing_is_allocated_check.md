# [Bug]: validate_thread_affinity 讀取 owner_thread 缺少 is_allocated 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 slot 被 sweep 後重新分配 |
| **Severity (嚴重程度)** | Medium | 讀取無效的 owner_thread 導致錯誤的執行緒安全判斷 |
| **Reproducibility (復現難度)** | Medium | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `validate_thread_affinity` in `cell.rs:257-289`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`validate_thread_affinity` 應該在讀取 `owner_thread` 之前檢查 slot 是否仍然被分配 (`is_allocated`)，以確保不會讀取已釋放並重新分配的 slot。

### 實際行為 (Actual Behavior)
函數直接讀取 `(*header.as_ptr()).owner_thread` (line 269)，沒有先檢查 `is_allocated`。如果 slot 已被 lazy sweep 釋放並重新分配，可能讀取到無效的 owner_thread 資料。

---

## 🔍 根因分析 (Root Cause Analysis)

在 `cell.rs:269`:
```rust
let owner = unsafe { (*header.as_ptr()).owner_thread };
```

缺少 `is_allocated` 檢查。當 slot 被釋放並重新分配時，讀取的 `owner_thread` 是無效資料。

此 bug 與以下已記錄的 bug 類似，但發生在不同的函數：
- Bug276: `get_allocating_thread_id` in `heap.rs` - 缺少 is_allocated 檢查
- Bug277: `gc_cell_validate_and_barrier` in `heap.rs` - 缺少 is_allocated 檢查

---

## 🧪 PoC (Proof of Concept)

需要設計能觸發以下條件的測試：
1. 建立 GcCell
2. 觸發 lazy sweep 釋放該 slot
3. 重新分配該 slot
4. 在新物件上調用 validate_thread_affinity

---

## 💡 建議修復 (Suggested Fix)

在讀取 `owner_thread` 之前添加 `is_allocated` 檢查：

```rust
let header = unsafe { crate::heap::ptr_to_page_header(cell_ptr) };

// 計算 index
let block_size = unsafe { (*header.as_ptr()).block_size } as usize;
let header_size = unsafe { (*header.as_ptr()).header_size } as usize;
let header_addr = header.as_ptr() as usize;
let offset = cell_ptr as usize - (header_addr + header_size);
let index = offset / block_size;

// 檢查 is_allocated
if !unsafe { (*header.as_ptr()).is_allocated(index) } {
    return; // slot 不再分配，跳過執行緒驗證
}

let owner = unsafe { (*header.as_ptr()).owner_thread };
```

---

## 💬 內部討論記錄 (Internal Discussion Record)

### R. Kent Dybvig
GC 需要確保所有 heap 指標操作都有適當的安全檢查。讀取物件元資料（如 owner_thread）前，必須確認 slot 仍然有效。

### Rustacean
這是一個經典的 TOCTOU 模式 - 讀取 owner_thread 與後續的斷言之間，slot 可能被釋放並重新分配。

### Geohot
攻擊者可能利用這個漏洞，通過精確時序控制來繞過執行緒安全檢查，雖然難度較高。

---

## Resolution (2026-03-15)

**Fixed.** Added `is_allocated(index)` check before reading `owner_thread` in `validate_thread_affinity` (cell.rs). Uses `ptr_to_object_index` to compute index; returns early if slot was swept or pointer is invalid. Full test suite and Clippy pass.
