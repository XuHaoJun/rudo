# [Bug]: simple_write_barrier 缺少 is_allocated 檢查 - 與 bug200/211 不同的 code path

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 函數標記為 #[allow(dead_code)]，目前未被使用，但為潛在缺陷 |
| **Severity (嚴重程度)** | High | 可能導致 dirty tracking 混亂，影響 GC 正確性 |
| **Reproducibility (重現難度)** | High | 需要啟用函數並精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `simple_write_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`simple_write_barrier` 函數在調用 `set_dirty(index)` 之前沒有檢查 `is_allocated`。這與 Bug 200 和 Bug 211 類似，但是發生在不同的 code path。

**Bug 200 涵蓋:**
- `unified_write_barrier` (heap.rs)
- `GcThreadSafeCell::generational_write_barrier` (cell.rs)

**Bug 211 涵蓋:**
- `gc_cell_validate_and_barrier` (heap.rs)

**本 Bug 涵蓋:**
- `simple_write_barrier` (heap.rs) - 標記為 `#[allow(dead_code)]`

### 預期行為 (Expected Behavior)
在設置 dirty flag 之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該調用 `set_dirty`。

### 實際行為 (Actual Behavior)
`simple_write_barrier` 有兩處缺少 `is_allocated` 檢查：

1. **heap.rs:2717-2722** - large object case:
```rust
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
// 沒有 is_allocated 檢查！
(*header.as_ptr()).set_dirty(index);
heap.add_to_dirty_pages(header);
```

2. **heap.rs:2741-2748** - small object case:
```rust
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
// 沒有 is_allocated 檢查！
(*h.as_ptr()).set_dirty(index);
heap.add_to_dirty_pages(h);
```

對比：正確的實作在 `gc/incremental.rs:1007-1010`:
```rust
// Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
if !(*h).is_allocated(idx) {
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 mutator 並發執行時：
1. 物件 A 在 slot `index` 被 sweep（釋放）
2. 物件 B 在同一個 slot 被重新分配
3. Mutator 寫入物件 B 的欄位
4. `simple_write_barrier` 計算相同的 `index`
5. **BUG:** 對物件 B 的 slot 調用 `set_dirty(index)` - 破壞 dirty tracking
6. 這可能導致不正確的 GC 行為或記憶體損壞

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `simple_write_barrier` 的兩處添加 `is_allocated` 檢查：

1. 在 large object path 的 `(*header.as_ptr()).set_dirty(index)` 之前添加:
```rust
// 檢查 slot 是否仍然被分配
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
```

2. 在 small object path 的 `(*h.as_ptr()).set_dirty(index)` 之前添加:
```rust
// 檢查 slot 是否仍然被分配
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 與 Bug 200 和 Bug 211 類似，但發生在不同的 code path (`simple_write_barrier`)。Write barrier 的 dirty tracking 對於 incremental GC 的正確性至關重要。

**Rustacean (Soundness 觀點):**
這可能導致 use-after-free 類型的問題。當 slot 被重用後，舊的 metadata（如 dirty flag）可能干擾新的物件。

**Geohot (Exploit 觀點):**
雖然函數目前標記為 dead code，但若未來啟用，可能成為攻擊向量。
