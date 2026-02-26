# [Bug]: mark_page_dirty_for_ptr 未處理大型物件導致 Vec<Gc<T>> 追蹤失敗

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 Vec<Gc<T>> 位於大型物件中時觸發 |
| **Severity (嚴重程度)** | High | 導致 GC 無法掃描大型物件中的 Gc 指標，可能導致記憶體洩露 |
| **Reproducibility (復現難度)** | Medium | 需要分配大型物件並在其中存儲 Vec<Gc<T>> |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_page_dirty_for_ptr()` in `heap.rs:3205-3220`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`mark_page_dirty_for_ptr()` 應該正確標記所有類型物件所在的頁面為 dirty，包括大型物件。這樣 GC 在標記階段才能掃描這些頁面中的 Gc 指標。

### 實際行為 (Actual Behavior)
`mark_page_dirty_for_ptr()` 只檢查 `small_pages`，完全忽略大型物件：

```rust
// heap.rs:3215-3218
if heap.small_pages.contains(&page_addr) {
    let header = unsafe { ptr_to_page_header(ptr) };
    unsafe { heap.add_to_dirty_pages(header) };
}
```

相比之下，`gc_cell_validate_and_barrier()` 正確處理大型物件：

```rust
// heap.rs:2682-2684
let (h, index) = if let Some(&(head_addr, size, h_size)) =
    heap.large_object_map.get(&page_addr)
{
    // 正確處理大型物件...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`mark_page_dirty_for_ptr()` 函數（`heap.rs:3205-3220`）的實現：

```rust
pub unsafe fn mark_page_dirty_for_ptr(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }

    let page_addr = ptr as usize & page_mask();

    HEAP.with(|local| {
        let heap = unsafe { &mut *local.tcb.heap.get() };

        if heap.small_pages.contains(&page_addr) {  // 只檢查小物件頁面！
            let header = unsafe { ptr_to_page_header(ptr) };
            unsafe { heap.add_to_dirty_pages(header) };
        }
    });
}
```

問題：當指標指向大型物件的頁面時，`small_pages.contains()` 返回 false，導致頁面不會被標記為 dirty。

這會影響 `trace.rs:325` 中 `Vec<Gc<T>>` 的追蹤：
```rust
// trace.rs:323-327
if !self.is_empty() {
    unsafe {
        crate::heap::mark_page_dirty_for_ptr(self.as_ptr().cast::<u8>());
    }
}
```

當 Vec<Gc<T>> 存儲在大型物件中時，其內部緩衝區的頁面不會被標記為 dirty，導致 GC 在標記階段無法掃描這些 Gc 指標。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 分配一個大於 MAX_SMALL_OBJECT_SIZE 的物件（大型物件）
2. 在大型物件中存儲一個 `Vec<Gc<T>>`
3. 觸發 GC 標記
4. 觀察 Vec 中的 Gc 指標是否被正確掃描

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `mark_page_dirty_for_ptr()` 以處理大型物件：

```rust
pub unsafe fn mark_page_dirty_for_ptr(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }

    let page_addr = ptr as usize & page_mask();

    HEAP.with(|local| {
        let heap = unsafe { &mut *local.tcb.heap.get() };

        // 檢查大型物件
        if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
            // 如果在大型物件範圍內，標記 head page 為 dirty
            let header = head_addr as *mut PageHeader;
            heap.add_to_dirty_pages(NonNull::new(header).unwrap());
            return;
        }

        // 檢查小物件頁面
        if heap.small_pages.contains(&page_addr) {
            let header = unsafe { ptr_to_page_header(ptr) };
            heap.add_to_dirty_pages(header);
        }
    });
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
大型物件使用單獨的追蹤機制（`large_object_map`），不同於小物件的頁面管理。當 GC 追蹤 Vec<Gc<T>> 時，需要確保所有類型的物件頁面都被正確標記為 dirty。

**Rustacean (Soundness 觀點):**
這不是傳統意義的記憶體不安全，但可能導致記憶體洩露 - 大型物件中的 Gc 指標未被追蹤，導致它們指向的物件被錯誤回收。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制大型物件的分配和釋放，可能利用此漏洞進行記憶體洩露攻擊。但目前看來，這更像是一個正確性問題而非安全性漏洞。

---

## Resolution

`mark_page_dirty_for_ptr()` 已於 heap.rs 支援大型物件：先檢查 `large_object_map`（thread-local 與 segment_manager 全域），若 ptr 在大型物件 value 範圍內，則將 head page 加入 dirty_pages。
