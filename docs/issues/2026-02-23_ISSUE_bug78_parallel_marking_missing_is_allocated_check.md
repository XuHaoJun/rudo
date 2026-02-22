# [Bug]: Parallel Marking 缺少 is_allocated 檢查 - 可能標記錯誤物件

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 parallel marking 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致標記錯誤的物件，造成記憶體損壞或洩漏 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Parallel Marking (`worker_mark_loop`, `worker_mark_loop_with_registry`, `mark_and_push_to_worker_queue`)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

在 parallel marking 的實作中，處理 work queue 中的物件時，只檢查 `is_marked()` 但沒有檢查 `is_allocated()`。這可能導致在 lazy sweep 與 marking 並發執行時，標記到錯誤的物件。

### 預期行為 (Expected Behavior)
在標記物件之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該標記。

### 實際行為 (Actual Behavior)
三個函數都缺少 `is_allocated` 檢查：

1. `gc/gc.rs:1212-1232` - `mark_and_push_to_worker_queue`:
```rust
if (*header.as_ptr()).is_marked(idx) {  // 只有 is_marked 檢查
    continue;
}
(*header.as_ptr()).set_mark(idx);  // 直接標記，沒有 is_allocated 檢查
```

2. `gc/marker.rs:885-925` - `worker_mark_loop`:
```rust
if (*header.as_ptr()).is_marked(idx) {  // 只有 is_marked 檢查
    continue;
}
(*header.as_ptr()).set_mark(idx);  // 直接標記，沒有 is_allocated 檢查
```

3. `gc/marker.rs:1009-1062` - `worker_mark_loop_with_registry`:
```rust
if (*header.as_ptr()).is_marked(idx) {  // 只有 is_marked 檢查
    continue;
}
(*header.as_ptr()).set_mark(idx);  // 直接標記，沒有 is_allocated 檢查
```

對比：其他正確實作（例如 `scan_page_for_marked_refs` in incremental.rs:771）有檢查：
```rust
if (*header).is_allocated(i) && !(*header).is_marked(i) {  // 正確：先檢查 is_allocated
    // ...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 incremental/parallel marking 並發執行時：
1. Marking 階段將物件加入 work queue
2. Lazy sweep 同時運行，收回死亡的 slot
3. Slot 被新的 allocation 重用
4. Worker 處理 queue 中的舊項目，嘗試標記
5. 由於沒有 `is_allocated` 檢查，會標記到新物件的 metadata
6. 這可能導致：
   - 錯誤地保留新物件（該物件實際上應該被回收）
   - 破壞 page metadata（標記位於已釋放 slot 的 bitmap）

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 啟用 parallel marking（`parallel_major_gc = true`）
3. 需要多執行緒環境：
   - Thread A: 進行 parallel marking
   - Thread B: 同時進行 lazy sweep，恰好重用同一個 slot
4. 時序條件：Thread A 處理 work queue 中的物件時，Thread B 剛好 sweep 並重用 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在所有三個函數中，添加 `is_allocated` 檢查：

```rust
// gc/gc.rs: mark_and_push_to_worker_queue
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box.as_ptr().cast()) {
    if !(*header.as_ptr()).is_allocated(idx) {  // 添加檢查
        return;
    }
    if !(*header.as_ptr()).is_marked(idx) {
        (*header.as_ptr()).set_mark(idx);
    }
}

// gc/marker.rs: worker_mark_loop 和 worker_mark_loop_with_registry
let Some(idx) = crate::heap::ptr_to_object_index(obj.cast()) else {
    continue;
};

if !(*header.as_ptr()).is_allocated(idx) {  // 添加檢查
    continue;
}

if (*header.as_ptr()).is_marked(idx) {
    continue;
}
(*header.as_ptr()).set_mark(idx);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Parallel marking 與 lazy sweep 的並發需要特別小心。當 slot 被重用時，必須確保標記操作不會影響到新的物件。Chez Scheme 的實現中，sweep 和 mark 是嚴格分離的，不允許並發。

**Rustacean (Soundness 觀點):**
這是一個潜在的 soundness 問題。如果標記到錯誤的物件，可能導致 use-after-free 或錯誤地保留已死亡的物件。

**Geohot (Exploit 觀點):**
攻擊者可能利用這個 race condition 來：
1. 阻止 GC 回收特定物件（透過錯誤標記）
2. 造成記憶體洩漏
3. 破壞 heap metadata 導致後續分配問題
