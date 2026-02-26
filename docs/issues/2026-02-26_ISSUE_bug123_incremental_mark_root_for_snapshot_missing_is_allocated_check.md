# [Bug]: Incremental Marking `mark_root_for_snapshot` 缺少 is_allocated 檢查

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 lazy sweep 與 snapshot phase 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致標記錯誤的物件，造成記憶體損壞或 UAF |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (`mark_root_for_snapshot` in `gc/incremental.rs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

在 incremental marking 的 snapshot phase 實作中，`mark_root_for_snapshot` 函數只檢查 `is_marked()` 但沒有檢查 `is_allocated()`。這與 bug78 修復的其他函數模式相同，但這個函數被遺漏了。

### 預期行為 (Expected Behavior)
在標記物件之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該標記。

### 實際行為 (Actual Behavior)
`mark_root_for_snapshot` (incremental.rs:567-574) 缺少 `is_allocated` 檢查：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
    let was_marked = (*header.as_ptr()).is_marked(idx);  // 只有 is_marked 檢查
    if !was_marked {
        (*header.as_ptr()).set_mark(idx);  // 缺少 is_allocated 檢查!
        visitor.objects_marked += 1;
    }
    visitor.worklist.push(ptr);
}
```

對比：正確實作 (`mark_object_black` in incremental.rs:970-984)：
```rust
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
        let header = crate::heap::ptr_to_page_header(ptr);
        let h = header.as_ptr();
        // 正確：先檢查 is_allocated
        if !(*h).is_allocated(idx) {
            return None;
        }
        if !(*h).is_marked(idx) {
            (*h).set_mark(idx);
            return Some(idx);
        }
    }
    None
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 incremental marking 的 snapshot phase 並發執行時：
1. Snapshot phase 開始，進行 stop-the-world
2. Lazy sweep 可能在同一 page 上運行，收回死亡的 slot
3. Slot 被新的 allocation 重用
4. `mark_root_for_snapshot` 嘗試標記 root，指針剛好指向新分配物件的 slot
5. 由於沒有 `is_allocated` 檢查，會標記到新物件的 metadata
6. 這可能導致：
   - 錯誤地保留新物件（該物件實際上應該被回收）
   - 破壞 page metadata（標記位於已釋放 slot 的 bitmap）
   - Potential UAF when drop runs concurrently

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 啟用 incremental marking
3. 需要多執行緒環境：
   - Thread A: 執行 snapshot phase 的 root 掃描
   - Thread B: 同時進行 lazy sweep，恰好重用同一個 slot
4. 時序條件：Thread A 處理 root 時，Thread B 剛好 sweep 並重用 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `mark_root_for_snapshot` 中添加 `is_allocated` 檢查：

```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
    // 添加檢查
    if !(*header.as_ptr()).is_allocated(idx) {
        return;
    }
    let was_marked = (*header.as_ptr()).is_marked(idx);
    if !was_marked {
        (*header.as_ptr()).set_mark(idx);
        visitor.objects_marked += 1;
    }
    visitor.worklist.push(ptr);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 bug78 的遺漏個案。Bug78 修復了 parallel marking 的三個函數，但 `mark_root_for_snapshot` 被遺漏了。Snapshot phase 理論上是 stop-the-world，但如果 lazy sweep 尚未完全完成，可能會有殘餘的並發。

**Rustacean (Soundness 觀點):**
這是潜在的 soundness 問題。標記到錯誤的物件可能導致 use-after-free 或錯誤地保留已死亡的物件。

**Geohot (Exploit 觀點):**
攻擊者可能利用這個 race condition 來：
1. 阻止 GC 回收特定物件（透過錯誤標記）
2. 造成記憶體洩漏
3. 破壞 heap metadata 導致後續分配問題
