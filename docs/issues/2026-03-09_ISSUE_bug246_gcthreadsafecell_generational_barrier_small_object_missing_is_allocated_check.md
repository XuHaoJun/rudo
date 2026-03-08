# [Bug]: GcThreadSafeCell::generational_write_barrier 小型物件路徑缺少 is_allocated 檢查

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 mutator 並發執行，物件槽位被重用 |
| **Severity (嚴重程度)** | High | 可能導致 dirty tracking 混亂，影響 GC 正確性 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::generational_write_barrier` (cell.rs:1223-1225)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`GcThreadSafeCell::generational_write_barrier` 函數在小型物件路徑中，調用 `set_dirty(index)` 之前沒有檢查 `is_allocated(index)`。這與 Bug 200 與 Bug 213 類似，但是發生在不同的 code path。

**相關 Bug 覆蓋範圍:**
- Bug 200: 聲稱覆蓋 `unified_write_barrier` + `generational_write_barrier` 小型物件路徑，但實際只修復了 `unified_write_barrier`
- Bug 213: 覆蓋 `generational_write_barrier` 大型物件路徑 (cell.rs:1205)
- **本 Bug 覆蓋:** `generational_write_barrier` 小型物件路徑 (cell.rs:1223-1225) - **之前未被任何 bug 修復！**

### 預期行為 (Expected Behavior)
在設置 dirty flag 之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該調用 `set_dirty`。

### 實際行為 (Actual Behavior)
`GcThreadSafeCell::generational_write_barrier` 小型物件路徑缺少 `is_allocated` 檢查：

**cell.rs:1223-1225** - 小型物件 case:
```rust
if index < (*header.as_ptr()).obj_count as usize {
    // 沒有 is_allocated 檢查！
    (*header.as_ptr()).set_dirty(index);
    heap.add_to_dirty_pages(header);
}
```

對比：正確的實作在 `heap.rs:2976-2979`:
```rust
// Skip if slot was swept; avoids corrupting dirty tracking with reused slot (bug200).
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 mutator 並發執行時：
1. 物件 A 在 slot 0 被 sweep（釋放）
2. 物件 B 在同一個 slot 被重新分配
3. Mutator 寫入物件 B 的欄位
4. `GcThreadSafeCell::generational_write_barrier` 計算相同的 index
5. **BUG:** 對物件 B 的 slot 調用 `set_dirty(index)` - 破壞 dirty tracking
6. 這可能導致不正確的 GC 行為或記憶體損壞

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcThreadSafeCell::generational_write_barrier` 的小型物件路徑添加 `is_allocated` 檢查：

```rust
if index < (*header.as_ptr()).obj_count as usize {
    // 檢查 slot 是否仍然被分配 (bug246)
    if !(*header.as_ptr()).is_allocated(index) {
        return;
    }
    (*header.as_ptr()).set_dirty(index);
    heap.add_to_dirty_pages(header);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 與 Bug 200、213 類似，但發生在小型物件 code path。Write barrier 的 dirty tracking 對於 incremental GC 的正確性至關重要。Bug 200 當初聲稱要修復這個路徑，但實際上並未實作。

**Rustacean (Soundness 觀點):**
這可能導致 use-after-free 類型的問題。當 slot 被重用後，舊的 metadata（如 dirty flag）可能干擾新的物件。

**Geohot (Exploit 觀點):**
攻擊者可以嘗試控制 slot 重用的時序，來操縱 dirty_pages 的內容。
