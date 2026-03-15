# [Bug]: gc_cell_validate_and_barrier 讀取 owner_thread 缺少 is_allocated 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 在 lazy sweep 後觸發 write barrier 時可能發生 |
| **Severity (嚴重程度)** | `High` | 讀取已釋放物件的 owner_thread 導致執行緒安全驗證錯誤 |
| **Reproducibility (復現難度)** | `Medium` | 需要時序控制來觸發 lazy sweep 和 barrier 的交錯 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `gc_cell_validate_and_barrier` (heap.rs:2821, 2848)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`gc_cell_validate_and_barrier` 在讀取 `owner_thread` 進行執行緒安全驗證前，應該先檢查物件是否已分配 (is_allocated)。

### 實際行為 (Actual Behavior)
當 lazy sweep 回收物件槽位後，write barrier 仍可能觸發並讀取已釋放物件的 `owner_thread`，導致執行緒安全驗證讀取到 garbage 值。

注意：此函數已在 line 2893-2896 修復了 `set_dirty` 的 `is_allocated` 檢查 (bug211)，但 `owner_thread` 的讀取發生在 line 2821/2848，仍早於 line 2893 的檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs` 的 `gc_cell_validate_and_barrier` 函數：

**Large object path (line 2821):**
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
let owner = (*h_ptr).owner_thread;  // <-- 缺少 is_allocated 檢查!
assert!(owner == 0 || owner == current, ...);
```

**Small object path (line 2848):**
```rust
// ... boundary checks ...
let owner = (*h).owner_thread;  // <-- 缺少 is_allocated 檢查!
assert!(owner == 0 || owner == current, ...);
```

後續在 line 2893 有 `is_allocated` 檢查，但此時 `owner_thread` 已經被讀取。

對比 `get_allocating_thread_id` (bug276) 有相同的問題模式：
```rust
// bug276 也缺少 is_allocated check
unsafe { (*header.as_ptr()).owner_thread }
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 分配 GcCell 物件 A
2. 觸發 lazy sweep 回收物件 A（使 slot 變為 unallocated）
3. 在同一 slot 重新分配物件 B（如果可能的話）
4. 對物件 B 進行 mutation，觸發 `gc_cell_validate_and_barrier`
5. 函數讀取 `owner_thread`（可能讀到物件 A 的 stale 值）
6. 執行緒安全 assert 可能失敗 或 讀取到 garbage

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在讀取 `owner_thread` 之前添加 `is_allocated` 檢查：

**Large object path (line 2821 之前):**
```rust
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_addr) {
    if !(*h_ptr).is_allocated(idx) {
        return;
    }
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
...
```

**Small object path (line 2848 之前):**
```rust
let offset = ptr_addr - (header_page_addr + header_size);
let index = offset / block_size;
let obj_count = (*h).obj_count as usize;
if index >= obj_count {
    return;
}
// 添加 is_allocated 檢查
if !(*h).is_allocated(index) {
    return;
}
let owner = (*h).owner_thread;
```

或者，更好的做法是將 `is_allocated` 檢查提前到讀取任何 page header field 之前。

---

## 💬 內部討論記錄 (Internal Discussion Record)

### R. Kent Dybvig (GC Expert)

此問題與 bug276 (`get_allocating_thread_id`) 為相同的模式問題，但發生在不同的函數。GC 元件在讀取任何 metadata 前都應該驗證物件的 allocation 狀態，否則會讀取到 stale/garbage 資料。

### Rustacean (Memory Safety Expert)

這是 release build 的 UB 風險。debug_assert 不足夠，需要實際的 runtime check。當 slot 被 sweep 後重用，`owner_thread` 可能包含任意記憶體內容。

### Geohot (Exploit/Edge Case Expert)

時序依賴：需要 lazy sweep 和 write barrier 的交錯。攻擊者可能嘗試控制 slot 重用時序來影響執行緒安全驗證邏輯。

---

## Resolution (2026-03-15)

**Fixed.** The large object path already had `is_allocated(0)` before `owner_thread` (bug247). The small object path read `owner_thread` before computing index and checking `is_allocated(index)`. Reordered the small object path: compute bounds and index first, add `is_allocated(index)` check, then read `owner_thread` and assert. All tests pass.