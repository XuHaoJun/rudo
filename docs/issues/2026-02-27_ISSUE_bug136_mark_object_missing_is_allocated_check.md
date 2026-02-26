# [Bug]: mark_object 缺少 is_allocated 檢查 - 處理 cross-thread SATB 時可能 UAF

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 cross-thread SATB 處理並發執行 |
| **Severity (嚴重程度)** | High | 可能導致錯誤標記已釋放的 slot，造成記憶體損壞或 UAF |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object` in `gc/gc.rs`, `mark_and_trace_incremental` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`mark_object` 函數在處理 cross-thread SATB buffer 時缺少 `is_allocated` 檢查。當 lazy sweep 與 incremental/parallel marking 並發執行時，可能會標記到已釋放並被重用的 slot。

### 預期行為 (Expected Behavior)

在標記物件之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該標記。這與 `mark_object_black` (incremental.rs:976) 和其他標記函數的行為一致。

### 實際行為 (Actual Behavior)

`mark_object` (gc/gc.rs:2332-2353) 缺少 `is_allocated` 檢查：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
    if (*header.as_ptr()).is_marked(idx) {  // 只有 is_marked 檢查
        return;
    }
    (*header.as_ptr()).set_mark(idx);  // 缺少 is_allocated 檢查!
}
```

對比：正確實作 (`mark_object_black` in incremental.rs:976-990):
```rust
// Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
if !(*h).is_allocated(idx) {
    return None;
}
if !(*h).is_marked(idx) {
    (*h).set_mark(idx);
    return Some(idx);
}
```

---

## 🔬 根本原因分析 (Root Root Cause Analysis)

當 lazy sweep 與 cross-thread SATB buffer 處理並發執行時：
1. Thread A: 透過 `GcThreadSafeCell::borrow_mut()` 或 `GcRwLock::write()` 進行 mutation
2. Thread A: 將舊的 GC 指針推入 cross-thread SATB buffer
3. Thread B: 同時進行 lazy sweep，收回該物件的 slot
4. Slot 被新的 allocation 重用
5. GC: 調用 `mark_object` 處理 cross-thread SATB buffer 中的條目
6. 由於沒有 `is_allocated` 檢查，會標記到新物件的 metadata
7. 這可能導致：
   - 錯誤地保留新物件（該物件實際上應該被回收）
   - 破壞 page metadata（標記到位於已釋放 slot 的 bitmap）
   - Potential UAF when accessing the incorrectly marked object

這與 bug78、bug108、bug123、bug129 類似，但它們修復了不同的函數。`mark_object` 和 `mark_and_trace_incremental` 被遺漏了。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 使用 `GcThreadSafeCell` 或 `GcRwLock` 從不同線程進行 mutation
3. 需要多執行緒環境：
   - Thread A: 透過 cross-thread 類型進行 mutation，觸發 SATB buffer
   - Thread B: 同時進行 lazy sweep，恰好重用同一個 slot
4. 時序條件：Thread A 處理 mutation 時，Thread B 剛好 sweep 並重用 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `gc/gc.rs` 的 `mark_object` 和 `mark_and_trace_incremental` 中添加 `is_allocated` 檢查：

```rust
pub unsafe fn mark_object(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let header = unsafe { crate::heap::ptr_to_page_header(ptr_addr) };

    unsafe {
        if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
            return;
        }

        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
            // 添加檢查：跳過已釋放的 slot
            if !(*header.as_ptr()).is_allocated(idx) {
                return;
            }
            if (*header.as_ptr()).is_marked(idx) {
                return;
            }
            (*header.as_ptr()).set_mark(idx);
            visitor.objects_marked += 1;
        } else {
            return;
        }

        visitor.worklist.push(ptr);
    }
}
```

同樣應用於 `mark_and_trace_incremental`。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 bug78、bug108、bug123、bug129 的遺漏個案。`mark_object` 和 `mark_and_trace_incremental` 函數用於處理 cross-thread SATB buffer 和一般標記，但在修復其他標記函數時被遺漏了。Snapshot-at-the-beginning (SATB) 需要確保所有在 snapshot 時存活的物件都被正確標記，並且不會標記到已釋放的 slot。

**Rustacean (Soundness 觀點):**
這是潛在的 soundness 問題。標記到錯誤的物件可能導致 use-after-free 或錯誤地保留已死亡的物件。在 concurrent 環境中，這種錯誤標記特別危險。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 阻止 GC 回收特定物件（透過錯誤標記）
2. 造成記憶體洩漏
3. 破壞 heap metadata 導致後續分配問題
4. 在極端情況下，可能實現 use-after-free
