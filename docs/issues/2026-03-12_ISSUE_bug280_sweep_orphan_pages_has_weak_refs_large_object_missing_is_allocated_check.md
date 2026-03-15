# [Bug]: sweep_orphan_pages has_weak_refs large object path 缺少 is_allocated 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 orphan large object 與 lazy sweep 交互 |
| **Severity (嚴重程度)** | Medium | 可能讀取已釋放物件的 weak_count，導致錯誤判斷 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序條件來觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `sweep_orphan_pages`, `heap.rs:3130-3135`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
在檢查 object 是否有 weak refs (`weak_count() > 0`) 之前，應該先檢查該 slot 是否仍然被分配 (`is_allocated`)，確保不會讀取已釋放並重用的 slot。

### 實際行為
`sweep_orphan_pages` 函數中，`has_weak_refs` 對於 large object 的處理缺少 `is_allocated` 檢查：

```rust
// heap.rs:3130-3135
let has_weak_refs = if is_large {
    let header_size = (*header).header_size as usize;
    let obj_ptr = (orphan.addr as *mut u8).add(header_size);
    #[allow(clippy::cast_ptr_alignment)]
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).weak_count() > 0  // 缺少 is_allocated 檢查!
} else {
    // ... 小型物件有 is_allocated 檢查 (lines 3141-3150)
};
```

對比同函數中小型物件的正確實作（lines 3141-3150）：
```rust
(0..obj_count).any(|i| {
    if (*header).is_allocated(i) {  // 正確：先檢查 is_allocated
        let obj_ptr = (orphan.addr as *mut u8).add(header_size + i * block_size);
        let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
        (*gc_box_ptr).weak_count() > 0
    } else {
        false
    }
})
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 運行時，orphan page 中的 slots 可能被釋放並重新分配：

1. Large object A 被標記為 orphan
2. Lazy sweep 回收了 object A 的 slot
3. 新的 object B 被分配到同一個 slot
4. `sweep_orphan_pages` 檢查 has_weak_refs
5. 沒有 is_allocated 檢查，導致讀取到 object B 的 weak_count
6. 這可能導致錯誤的判斷：原本應該被回收的 orphan page 被錯誤保留

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 lazy sweep feature
2. 分配一個 large object，成為 orphan（透過 cross-thread handle）
3. 觸發 GC，讓 object 被標記
4. 同時進行 lazy sweep 來重用 orphan page 中的 slot
5. 在 slot 中分配新物件
6. 呼叫 `sweep_orphan_pages`
7. 觀察 has_weak_refs 返回錯誤的值

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `has_weak_refs` 的 large object 路径中添加 `is_allocated` 驗證：

```rust
let has_weak_refs = if is_large {
    // Large objects: check is_allocated first
    if !(*header).is_allocated(0) {
        false
    } else {
        let header_size = (*header).header_size as usize;
        let obj_ptr = (orphan.addr as *mut u8).add(header_size);
        #[allow(clippy::cast_ptr_alignment)]
        let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
        (*gc_box_ptr).weak_count() > 0
    }
} else {
    // ... existing code
};
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

### R. Kent Dybvig (GC Expert)

這與 bug126 為相同模式問題的另一個實例。Large object 路徑和 small object 路徑應該保持一致的行為 - 在讀取任何 GcBox 欄位之前都應該驗證 is_allocated。

### Rustacean (Memory Safety Expert)

這可能導致記憶體錯誤讀取 - 讀取已被釋放物件的 weak_count 值，可能導致不正確的判斷。

### Geohot (Exploit/Edge Case Expert)

時序依賴：需要 lazy sweep 和 orphan sweep 的交錯。攻擊者可能嘗試控制 slot 重用時序來影響 GC 行為。

---

## Resolution (2026-03-15)

**Outcome:** Fixed and verified.

Added `is_allocated(0)` check before reading `weak_count_acquire()` in the large object path of `sweep_orphan_pages` (`heap.rs`). Behavior now matches the small object path. Full test suite passes.
