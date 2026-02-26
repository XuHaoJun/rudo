# [Bug]: GcVisitorConcurrent::route_reference 缺少 is_allocated 檢查導致錯誤標記

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 lazy sweep 與 concurrent marking 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致錯誤標記，造成記憶體損壞或 UAF |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcVisitorConcurrent::route_reference` in `trace.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

在標記物件之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該標記。這與其他標記函數（如 `gc/marker.rs` 中的函數）的行為一致。

### 實際行為 (Actual Behavior)

`GcVisitorConcurrent::route_reference` (`trace.rs:174-182`) 缺少 `is_allocated` 檢查：

```rust
if let Some(idx) = super::heap::ptr_to_object_index(raw.cast()) {
    if self.kind == VisitorKind::Minor && (*header.as_ptr()).generation > 0 {
        return;
    }

    if (*header.as_ptr()).is_marked(idx) {  // 只有 is_marked 檢查
        return;
    }
    (*header.as_ptr()).set_mark(idx);  // 缺少 is_allocated 檢查!
}
```

對比：正確實作 (`gc/marker.rs:907-914`)：
```rust
if !(*header.as_ptr()).is_allocated(idx) {  // 正確：先檢查 is_allocated
    continue;
}
if (*header.as_ptr()).is_marked(idx) {
    continue;
}

(*header.as_ptr()).set_mark(idx);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 concurrent marking 並發執行時：
1. Concurrent marking 正在掃描參考
2. Lazy sweep 在同一 page 上運行，收回死亡的 slot
3. Slot 被新的 allocation 重用
4. `route_reference` 嘗試標記 root，指針剛好指向新分配物件的 slot
5. 由於沒有 `is_allocated` 檢查，會標記到新物件的 metadata
6. 這可能導致：
   - 錯誤地保留新物件（該物件實際上應該被回收）
   - 破壞 page metadata（標記位於已釋放 slot 的 bitmap）
   - Potential UAF when drop runs concurrently

這是 bug78 和 bug123 的遺漏個案。`route_reference` 函數被遺漏了。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 啟用 concurrent marking
3. 需要多執行緒環境：
   - Thread A: 執行 concurrent marking 的 reference routing
   - Thread B: 同時進行 lazy sweep，恰好重用同一個 slot
4. 時序條件：Thread A 處理 reference 時，Thread B 剛好 sweep 並重用 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `trace.rs` 的 `route_reference` 中添加 `is_allocated` 檢查：

```rust
if let Some(idx) = super::heap::ptr_to_object_index(raw.cast()) {
    if self.kind == VisitorKind::Minor && (*header.as_ptr()).generation > 0 {
        return;
    }

    // 添加檢查
    if !(*header.as_ptr()).is_allocated(idx) {
        return;
    }

    if (*header.as_ptr()).is_marked(idx) {
        return;
    }
    (*header.as_ptr()).set_mark(idx);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 bug78 和 bug123 的遺漏個案。`GcVisitorConcurrent::route_reference` 函數用於 concurrent marking 的 reference routing，但在修復 parallel marking 和 incremental marking 時被遺漏了。Snapshot-at-the-beginning (SATB) 需要確保所有在 snapshot 時存活的物件都被正確標記。

**Rustacean (Soundness 觀點):**
這是潜在的 soundness 問題。標記到錯誤的物件可能導致 use-after-free 或錯誤地保留已死亡的物件。在 concurrent 環境中，這種錯誤標記特別危險。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 阻止 GC 回收特定物件（透過錯誤標記）
2. 造成記憶體洩漏
3. 破壞 heap metadata 導致後續分配問題
4. 在極端情況下，可能實現 use-after-free

---

## Resolution (2026-02-27)

**Outcome:** Fixed.

Added `is_allocated(idx)` check to `GcVisitorConcurrent::route_reference` in `trace.rs` before `is_marked`/`set_mark`, matching the pattern used in `gc/marker.rs` (bug78, bug123). This prevents marking slots that have been swept and reused when lazy sweep runs concurrently with marking.

---
