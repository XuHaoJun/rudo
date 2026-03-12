# [Bug]: get_allocating_thread_id 缺少 is_allocated check，讀取已釋放物件的 owner_thread

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 lazy sweep 後觸發 write barrier 時可能發生 |
| **Severity (嚴重程度)** | High | 讀取已釋放物件的 owner_thread 導致 SATB barrier 錯誤路由 |
| **Reproducibility (復現難度)** | Medium | 需要時序控制來觸發 lazy sweep 和 barrier 的交錯 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap::get_allocating_thread_id`, `record_satb_old_value`
- **OS / Architecture:** Linux x86_64, All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`get_allocating_thread_id` 函數在讀取 `owner_thread` 前沒有檢查物件是否已分配 (is_allocated)。僅在 debug build 有 `debug_assert` 檢查，release build 缺少此檢查。

### 預期行為
在讀取 `owner_thread` 前，應該先檢查物件是否為 allocated 狀態，避免讀取已被 sweep 的物件資料。

### 實際行為
Release build 中，當物件已被 lazy sweep 回收後，write barrier 仍然可能呼叫 `get_allocating_thread_id`，導致讀取到 garbage 或 stale 的 owner_thread 值，進而影響 SATB barrier 的 routing 邏輯。

---

## 🔬 根本原因分析 (Root Cause Analysis)

位於 `crates/rudo-gc/src/heap.rs:66-94` 的 `get_allocating_thread_id` 函數：

```rust
pub(crate) unsafe fn get_allocating_thread_id(gc_box_addr: usize) -> u64 {
    // ... heap bounds check ...
    
    let header = unsafe { ptr_to_page_header(gc_box_addr as *const u8) };

    if let Some(idx) = unsafe { ptr_to_object_index(gc_box_addr as *const u8) } {
        debug_assert!(
            unsafe { (*header.as_ptr()).is_allocated(idx) },
            "Reading owner_thread from potentially free'd object at index {idx}"
        );
        // ...其他 debug_assert ...
    }

    unsafe { (*header.as_ptr()).owner_thread }  // <-- 缺少 is_allocated check!
}
```

問題：
1. `debug_assert` 只在 debug build 執行
2. Release build 缺少 `is_allocated` 檢查
3. 當物件被 lazy sweep 後，`owner_thread` 可能為 garbage

此函數被 `record_satb_old_value` (line 1934) 呼叫，發生在 write barrier 期間。如果此時物件已被 sweep，會讀取錯誤的 allocating_thread_id。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要構造以下時序：
1. 分配一個 GcCell 物件
2. 觸發 lazy sweep 回收該物件 (或使其變為 unallocated)
3. 再次 mutate 該 GcCell，觸發 write barrier
4. write barrier 呼叫 `record_satb_old_value` -> `get_allocating_thread_id`
5. 讀取到錯誤的 owner_thread

```rust
// PoC 需要時序控制，請參考 bug273 的測試模式
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `get_allocating_thread_id` 中，讀取 `owner_thread` 前新增 `is_allocated` 檢查：

```rust
pub(crate) unsafe fn get_allocating_thread_id(gc_box_addr: usize) -> u64 {
    // ... existing bounds check ...
    
    let header = unsafe { ptr_to_page_header(gc_box_addr as *const u8) };

    if let Some(idx) = unsafe { ptr_to_object_index(gc_box_addr as *const u8) } {
        // 新增: Release build 也需要檢查
        if !unsafe { (*header.as_ptr()).is_allocated(idx) } {
            return 0;  // 物件已被回收，回傳 0 (視為無效)
        }
        // ... existing assertions ...
    }

    unsafe { (*header.as_ptr()).owner_thread }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 讀取已釋放物件的 metadata 違反了 GC 的健全性原則
- owner_thread 錯誤會導致 SATB barrier 將指標錯誤地分類為 cross-thread 或 local，影響 barrier 的正確性
- 類似於 bug273 的模式，但發生在不同函數

**Rustacean (Soundness 觀點):**
- Release build 缺少 safety check 是 UB 風險
- debug_assert 在 release build 被移除，導致原本在 debug 發現的問題在 release 隱藏
- 建議將關鍵的安全檢查從 debug_assert 提升為實際的 runtime check

**Geohot (Exploit 觀點):**
- 讀取已釋放記憶體 (dangling pointer) 可能導致資訊洩漏
- 如果攻擊者能控制 heap layout，可能利用此漏洞進行 heap inspection
- 時序依賴性使得穩定利用困難，但概念上可攻擊