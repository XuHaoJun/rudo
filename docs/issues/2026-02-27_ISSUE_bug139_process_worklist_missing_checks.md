# [Bug]: process_worklist 缺少 is_allocated 和 is_marked 檢查 - 可能標記已釋放 slot

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 marking 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致錯誤標記已釋放的 slot，造成記憶體損壞或 UAF |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `process_worklist` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`process_worklist` 函數在處理 worklist 中的 GC 指標時缺少 `is_allocated` 和 `is_marked` 檢查。當 lazy sweep 與 marking 並發執行時，可能會標記到已釋放並被重用的 slot。

這與 bug137 (`GcVisitor::visit`) 不同的函數，且問題更嚴重：不僅缺少 `is_allocated` 檢查，連 `is_marked` 檢查也缺少！

### 預期行為 (Expected Behavior)

在標記物件之前，應該先檢查：
1. 該 slot 是否仍然被分配 (`is_allocated`)
2. 該物件是否已經被標記 (`is_marked`)

如果 slot 已被 sweep 釋放，或物件已經被標記，則不應該再標記。

### 實際行為 (Actual Behavior)

`process_worklist` (gc/gc.rs:2910-2930) 缺少這兩個檢查：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
    (*header.as_ptr()).set_mark(idx);  // 缺少 is_allocated AND is_marked 檢查!
    self.objects_marked += 1;
} else {
    continue;
}
```

對比：正確實作 (`GcVisitorConcurrent::route_reference` in trace.rs:174-185):
```rust
if let Some(idx) = super::heap::ptr_to_object_index(raw.cast()) {
    if !(*header.as_ptr()).is_allocated(idx) {  // 應該有這個檢查
        return;
    }
    if (*header.as_ptr()).is_marked(idx) {  // 應該有這個檢查
        return;
    }
    (*header.as_ptr()).set_mark(idx);
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 marking 並發執行時：
1. Thread A: 將 GC 指針添加到 worklist
2. Thread B: 同時進行 lazy sweep，收回該物件的 slot
3. Slot 被新的 allocation 重用
4. GC: 調用 `process_worklist` 處理 worklist 中的條目
5. 由於沒有 `is_allocated` 檢查，會標記到新物件的 metadata
6. 由於沒有 `is_marked` 檢查，會重複標記浪費效能
7. 這可能導致：
   - 錯誤地保留新物件（該物件實際上應該被回收）
   - 破壞 page metadata（標記到位於已釋放 slot 的 bitmap）
   - Potential UAF when accessing the incorrectly marked object

這與 bug137 (`GcVisitor::visit`) 類似，但 `process_worklist` 是單獨的函數，且問題更嚴重（缺少兩個檢查）。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 使用 `GcCell` 或類似類型進行 mutation
3. 需要多執行緒環境：
   - Thread A: 進行 mutation，觸發 worklist 添加
   - Thread B: 同時進行 lazy sweep，恰好重用同一個 slot
4. 時序條件：Thread A 處理 worklist 時，Thread B 剛好 sweep 並重用 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `gc/gc.rs` 的 `process_worklist` 中添加 `is_allocated` 和 `is_marked` 檢查：

```rust
pub fn process_worklist(&mut self) {
    while let Some(ptr) = self.worklist.pop() {
        unsafe {
            let ptr_addr = ptr.as_ptr() as *const u8;
            let header = crate::heap::ptr_to_page_header(ptr_addr);

            if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
                continue;
            }

            if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
                // 添加檢查：跳過已釋放的 slot
                if !(*header.as_ptr()).is_allocated(idx) {
                    continue;
                }
                // 添加檢查：跳過已標記的物件
                if (*header.as_ptr()).is_marked(idx) {
                    continue;
                }
                (*header.as_ptr()).set_mark(idx);
                self.objects_marked += 1;
            } else {
                continue;
            }

            ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), self);
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`process_worklist` 是標記階段的核心函數，負責處理 worklist 中的所有 GC 指紋。與 `GcVisitor::visit` (bug137) 不同，這是一個獨立的函數且問題更嚴重：缺少兩個關鍵檢查。正確的標記需要確保只標記有效且尚未標記的物件。

**Rustacean (Soundness 觀點):**
這是潛在的 soundness 問題。標記到錯誤的物件可能導致 use-after-free 或錯誤地保留已死亡的物件。缺少 `is_marked` 檢查也會導致重複標記浪費效能。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 阻止 GC 回收特定物件（透過錯誤標記）
2. 造成記憶體洩漏
3. 破壞 heap metadata 導致後續分配問題
4. 在極端情況下，可能實現 use-after-free

---

## Resolution (2026-03-01)

**Outcome:** Fixed (together with bug137).

Added `is_allocated(idx)` and `is_marked(idx)` guards in `process_worklist` (`gc/gc.rs`) before the `set_mark` call and the `trace_fn` invocation. This prevents calling `trace_fn` on a slot that was reclaimed by lazy sweep between the time the item was enqueued and the time it was dequeued. Also eliminates the double-counting in `objects_marked` that occurred when an object was marked by `GcVisitor::visit` (which already called `set_mark`) and then re-marked in `process_worklist`. Full test suite passes; clippy clean.
