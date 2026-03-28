# [Bug]: incremental_write_barrier missing second is_allocated check before record_in_remembered_buffer

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep and slot reuse between is_allocated check and barrier call |
| **Severity (嚴重程度)** | High | Could corrupt remembered set with swept slot, affecting GC correctness |
| **Reproducibility (復現難度)** | Medium | Requires precise concurrent timing between lazy sweep and mutator |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `incremental_write_barrier` (heap.rs), `gc_cell_validate_and_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`incremental_write_barrier` 應該在調用 `record_in_remembered_buffer` 之前有第二個 `is_allocated` 檢查，與 `simple_write_barrier` 的模式一致（bug212 修復）。

### 實際行為
`simple_write_barrier` 在 `set_dirty` 之前有第二個 `is_allocated` 檢查（lines 2867-2870），但 `incremental_write_barrier` 在 `record_in_remembered_buffer` 之前缺少等效的檢查。同樣的問題也存在於 `gc_cell_validate_and_barrier`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**檔案:** `crates/rudo-gc/src/heap.rs`

**`simple_write_barrier` 小型物件路徑 (lines 2856-2873):**
```rust
// 第一次 is_allocated 檢查 (line 2856)
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
(h, index)
};  // <-- 返回後

// 第二次 is_allocated 檢查 (lines 2867-2870) - bug212 修復
if !(*header.as_ptr()).is_allocated(index) {
    return;
}

(*header.as_ptr()).set_dirty(index);  // line 2872
heap.add_to_dirty_pages(header);
```

**`incremental_write_barrier` 小型物件路徑 (lines 3154-3189):**
```rust
// 只有一次 is_allocated 檢查 (line 3176)
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
(h, index)
};  // <-- 返回後

// 沒有第二次 is_allocated 檢查！
heap.record_in_remembered_buffer(header);  // line 3189 - 可能對已 sweep 的 slot 調用
```

**不一致:**
- `simple_write_barrier` 有第二次 `is_allocated` 檢查 (bug212)
- `incremental_write_barrier` 缺少等效檢查

**TOCTOU 場景:**
1. Slot 通過第一次 `is_allocated` 檢查 (line 3176)
2. Slot 被 lazy sweep 回收
3. 讀取 `has_gen_old` (可能從已釋放記憶體)
4. `has_gen_old` 為 true（如果物件曾經是 old generation），不通過 early return
5. 對已回收的 slot 調用 `record_in_remembered_buffer`！

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 理論場景 - 需要精確的並發時序
// 1. 分配 old generation 物件（has_gen_old = true）
// 2. 物件被 sweep 回收（generation = 0, has_gen_old 可能仍為 true）
// 3. Mutator 觸發 write barrier
// 4. 第一次 is_allocated 檢查通過（slot 尚未標記為 unallocated）
// 5. Slot 被 sweep - is_allocated 變為 false
// 6. 讀取 has_gen_old（可能為 true）
// 7. has_gen_old = true，early return 不觸發
// 8. record_in_remembered_buffer 對已回收 slot 調用！
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `incremental_write_barrier` 的 `record_in_remembered_buffer` 調用之前添加第二個 `is_allocated` 檢查：

```rust
};  // End of branch, return header

// 第二次 is_allocated 檢查 - 防止 TOCTOU
if !(*header.as_ptr()).is_allocated(index) {
    return;
}

heap.record_in_remembered_buffer(header);
```

同樣應用於 `gc_cell_validate_and_barrier` 的 `set_dirty` 調用之前。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Consistency in barrier implementation is critical for GC correctness. The bug212 fix added a second is_allocated check to simple_write_barrier, but the same pattern should be applied to incremental_write_barrier and gc_cell_validate_and_barrier for consistency.

**Rustacean (Soundness 觀點):**
Reading has_gen_old from a potentially swept slot and then proceeding to call record_in_remembered_buffer based on that value is problematic. Even if the early return condition provides some protection, the inconsistent handling across barrier functions is a code smell and potential bug.

**Geohot (Exploit 觀點):**
If an attacker can influence allocation patterns and GC timing, they might be able to cause the remembered set to contain invalid entries, potentially leading to GC correctness issues or memory corruption.

---

## 🔗 相關 Issue

- bug212: simple_write_barrier missing second is_allocated check - fixed
- bug286: barrier functions has_gen_old_flag ordering - partial fix applied

---

## 驗證記錄

**驗證日期:** 2026-03-21

**驗證方法:**
- Static code analysis comparing barrier functions
- `simple_write_barrier` (lines 2798-2876): Has second is_allocated check at lines 2867-2870
- `incremental_write_barrier` (lines 3113-3192): Missing second is_allocated check
- `gc_cell_validate_and_barrier` (lines 2889-3016): Missing second is_allocated check

**Status:** Issue confirmed - fix needed for consistency with bug212 pattern.

## Resolution (2026-03-21)

**Outcome:** Fixed.

**Changes made:**

1. `incremental_write_barrier` (heap.rs:3139):
   - Changed `let (header, _index)` to `let (header, index)` to bind index
   - Added second is_allocated check at lines 3194-3197 before `record_in_remembered_buffer`

2. `gc_cell_validate_and_barrier` (heap.rs:3004-3007):
   - Added second is_allocated check before `set_dirty` at lines 3006-3009

This makes `incremental_write_barrier` and `gc_cell_validate_and_barrier` consistent with `simple_write_barrier` which already had the bug212 fix.