# [Bug]: mark_object_minor 缺少 is_allocated 檢查 - 與 mark_object (bug136) 相同的模式

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 minor GC 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致錯誤標記已釋放的 slot，造成記憶體損壞或 UAF |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_minor` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`mark_object_minor` 函數在處理 Minor GC 標記時缺少 `is_allocated` 檢查。當 lazy sweep 與 minor GC 並發執行時，可能會標記到已釋放並被重用的 slot。

這與 bug136 (`mark_object`) 和 bug137 (`GcVisitor::visit`) 相同的模式，但影響不同的函數。

### 預期行為 (Expected Behavior)

在標記物件之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 釋放，則不應該標記。這與 `mark_object` (bug136) 和其他標記函數的行為一致。

### 實際行為 (Actual Behavior)

`mark_object_minor` (gc/gc.rs:2055-2084) 缺少 `is_allocated` 檢查：
```rust
pub unsafe fn mark_object_minor(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    // ...
    if (*header.as_ptr()).is_marked(index) {  // 只有 is_marked 檢查
        return;
    }
    (*header.as_ptr()).set_mark(index);  // 缺少 is_allocated 檢查!
    visitor.objects_marked += 1;
    // ...
}
```

對比：正確實作 (`mark_object` in gc/gc.rs:2341-2346，經 bug136 修復後)：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
    if !(*header.as_ptr()).is_allocated(idx) {  // 應該有這個檢查
        return;
    }
    if (*header.as_ptr()).is_marked(idx) {
        return;
    }
    (*header.as_ptr()).set_mark(idx);
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 minor GC 並發執行時：
1. Thread A: 呼叫 `find_gc_box_from_ptr`，返回 Some (物件已分配)
2. Thread B: 同時進行 lazy sweep，收回該物件的 slot (is_allocated 變為 false)
3. Slot 被新的 allocation 重用
4. Thread A: 呼叫 `mark_object_minor` 處理 GC 指針
5. 由於沒有 `is_allocated` 檢查，會標記到新物件的 metadata
6. 這可能導致：
   - 錯誤地保留新物件（該物件實際上應該被回收）
   - 破壞 page metadata（標記到位於已釋放 slot 的 bitmap）
   - Potential UAF when accessing the incorrectly marked object

這與 bug136、bug137 相同模式的遺漏個案。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 使用 `GcCell` 或類似類型進行 mutation
3. 需要多執行緒環境：
   - Thread A: 進行 mutation，觸發 Minor GC，呼叫 `mark_object_minor`
   - Thread B: 同時進行 lazy sweep，恰好重用同一個 slot
4. 時序條件：Thread A 處理 mutation 時，Thread B 剛好 sweep 並重用 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `gc/gc.rs` 的 `mark_object_minor` 中添加 `is_allocated` 檢查：

```rust
pub unsafe fn mark_object_minor(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let page_addr = (ptr_addr as usize) & crate::heap::page_mask();
    let header = unsafe { crate::heap::ptr_to_page_header(ptr_addr) };

    unsafe {
        if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
            return;
        }

        let block_size = (*header.as_ptr()).block_size as usize;
        let header_size = PageHeader::header_size(block_size);
        let data_start = page_addr + header_size;
        let offset = ptr_addr as usize - data_start;
        let index = offset / block_size;

        // 添加檢查：跳過已釋放的 slot
        if !(*header.as_ptr()).is_allocated(index) {
            return;
        }

        if (*header.as_ptr()).is_marked(index) {
            return;
        }

        (*header.as_ptr()).set_mark(index);
        visitor.objects_marked += 1;

        if (*header.as_ptr()).generation > 0 {
            return;
        }

        visitor.worklist.push(ptr);
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 bug136、bug137 的遺漏個案。`mark_object_minor` 是用於 Minor GC 標記的主要函數，但在修復其他標記函數時被遺漏了。Snapshot-at-the-beginning (SATB) 需要確保所有在 snapshot 時存活的物件都被正確標記，並且不會標記到已釋放的 slot。Minor GC 與 lazy sweep 的並發同樣需要這種保護。

**Rustacean (Soundness 觀點):**
這是潛在的 soundness 問題。標記到錯誤的物件可能導致 use-after-free 或錯誤地保留已死亡的物件。在 concurrent 環境中，這種錯誤標記特別危險。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 阻止 GC 回收特定物件（透過錯誤標記）
2. 造成記憶體洩漏
3. 破壞 heap metadata 導致後續分配問題
4. 在極端情況下，可能實現 use-after-free
