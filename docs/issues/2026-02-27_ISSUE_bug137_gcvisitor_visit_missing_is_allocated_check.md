# [Bug]: GcVisitor::visit 缺少 is_allocated 檢查 - 可能標記已釋放 slot

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 concurrent marking 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致錯誤標記已釋放的 slot，造成記憶體損壞或 UAF |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcVisitor::visit` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcVisitor::visit` 函數在處理 GC 指標時缺少 `is_allocated` 檢查。當 lazy sweep 與 marking 並發執行時，可能會標記到已釋放並被重用的 slot。

### 預期行為 (Expected Behavior)

在標記物件之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 釋放，則不應該標記。這與 `GcVisitorConcurrent::route_reference` (bug129) 和其他標記函數的行為一致。

### 實際行為 (Actual Behavior)

`GcVisitor::visit` (gc/gc.rs:2955-2960) 缺少 `is_allocated` 檢查：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
    if (*header.as_ptr()).is_marked(idx) {  // 只有 is_marked 檢查
        return;
    }
    (*header.as_ptr()).set_mark(idx);  // 缺少 is_allocated 檢查!
    self.objects_marked += 1;
```

對比：正確實作 (`GcVisitorConcurrent::route_reference` in trace.rs:174-182):
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
if (*header.as_ptr()).is_marked(idx) {
    return;
}
(*header.as_ptr()).set_mark(idx);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 marking 並發執行時：
1. Thread A: 進行 GC mutation，透過 `GcVisitor::visit` 處理 GC 指針
2. Thread B: 同時進行 lazy sweep，收回該物件的 slot
3. Slot 被新的 allocation 重用
4. GC: 調用 `GcVisitor::visit` 處理 GC 指針
5. 由於沒有 `is_allocated` 檢查，會標記到新物件的 metadata
6. 這可能導致：
   - 錯誤地保留新物件（該物件實際上應該被回收）
   - 破壞 page metadata（標記到位於已釋放 slot 的 bitmap）
   - Potential UAF when accessing the incorrectly marked object

這與 bug78、bug108、bug123、bug129、bug136 類似，但它們修復了不同的函數。`GcVisitor::visit` 被遺漏了。

注意：`process_worklist` (gc/gc.rs:2920-2925) 也有相同的問題，不僅缺少 `is_allocated` 檢查，也缺少 `is_marked` 檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 使用 `GcCell` 或類似類型進行 mutation
3. 需要多執行緒環境：
   - Thread A: 進行 mutation，觸發 `GcVisitor::visit`
   - Thread B: 同時進行 lazy sweep，恰好重用同一個 slot
4. 時序條件：Thread A 處理 mutation 時，Thread B 剛好 sweep 並重用 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `gc/gc.rs` 的 `GcVisitor::visit` 中添加 `is_allocated` 檢查：

```rust
impl Visitor for GcVisitor {
    #[inline]
    fn visit<T: Trace>(&mut self, gc: &crate::Gc<T>) {
        let raw = gc.raw_ptr();
        if !raw.is_null() {
            let ptr = raw.cast::<crate::ptr::GcBox<()>>();

            unsafe {
                let ptr_addr = ptr as *const u8;
                let header = crate::heap::ptr_to_page_header(ptr_addr);

                if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
                    return;
                }

                if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
                    // 添加檢查：跳過已釋放的 slot
                    if !(*header.as_ptr()).is_allocated(idx) {
                        return;
                    }
                    if (*header.as_ptr()).is_marked(idx) {
                        return;
                    }
                    (*header.as_ptr()).set_mark(idx);
                    self.objects_marked += 1;

                    if self.kind == VisitorKind::Minor && (*header.as_ptr()).generation > 0 {
                        return;
                    }
                } else {
                    return;
                }

                self.worklist.push(std::ptr::NonNull::new_unchecked(ptr));
            }
        }
    }
```

同樣應用於 `process_worklist` 函數。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 bug78、bug108、bug123、bug129、bug136 的遺漏個案。`GcVisitor::visit` 是用於處理 GC 指針的主要 Visitor 實作，但在修復其他標記函數時被遺漏了。Snapshot-at-the-beginning (SATB) 需要確保所有在 snapshot 時存活的物件都被正確標記，並且不會標記到已釋放的 slot。

**Rustacean (Soundness 觀點):**
這是潛在的 soundness 問題。標記到錯誤的物件可能導致 use-after-free 或錯誤地保留已死亡的物件。在 concurrent 環境中，這種錯誤標記特別危險。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 阻止 GC 回收特定物件（透過錯誤標記）
2. 造成記憶體洩漏
3. 破壞 heap metadata 導致後續分配問題
4. 在極端情況下，可能實現 use-after-free

---

## Resolution (2026-03-01)

**Outcome:** Fixed.

Added `is_allocated(idx)` guard in `GcVisitor::visit` (`gc/gc.rs`) before the existing `is_marked` check, matching the pattern used in every other marking helper (`mark_object`, `mark_and_trace_incremental`, `mark_root_for_snapshot`, `GcVisitorConcurrent::route_reference`). The guard returns early (null Weak-style) without marking or pushing the pointer when the slot has been reclaimed by lazy sweep. Bug139 (`process_worklist`) was also fixed in the same commit — `is_allocated` and `is_marked` guards added before `set_mark` and `trace_fn` calls. Full test suite passes; clippy clean.
