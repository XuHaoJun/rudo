# [Bug]: `gc_cell_validate_and_barrier` 小型物件路徑缺少第三個 `is_allocated` 檢查

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 lazy sweep 並發運行時可能觸發 |
| **Severity (嚴重程度)** | High | 可能導致錯誤物件被標記，進而導致 UAF 或不正確的 GC |
| **Reproducibility (復現難度)** | Medium | 需要並發場景，但可以通過 stress test 復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap::gc_cell_validate_and_barrier` (small object path)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`gc_cell_validate_and_barrier` 函數的小型物件路徑在讀取 `has_gen_old` flag 後，**缺少**第三個 `is_allocated` 檢查。相比之下，`incremental_write_barrier` 和 `simple_write_barrier` 都有這個檢查。

### 預期行為 (Expected Behavior)

在讀取 `has_gen_old` flag 後，應該再次檢查 `is_allocated` 以確保 slot 仍然有效，避免在 lazy sweep reclaim slot 並 reuse 後讀取新物件的 flag。

### 實際行為 (Actual Behavior)

小型物件路徑（line ~3095）在讀取 `has_gen_old` 後沒有 `is_allocated` 檢查就返回，而 `incremental_write_barrier` 和 `simple_write_barrier` 在相同操作後有第三個檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc_cell_validate_and_barrier` (heap.rs:3083-3095):

```rust
// GEN_OLD early-exit: skip only if page young AND object has no gen_old_flag (bug71).
// Cache flag to avoid TOCTOU between check and barrier (bug114).
// Second is_allocated check before reading has_gen_old - prevents TOCTOU (bug459, bug464)
if !(*h).is_allocated(index) {
    return;
}
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
(header, index)  // ← No is_allocated check before returning
```

對比 `incremental_write_barrier` (heap.rs:3305-3310) 有正確的第三個檢查：
```rust
// FIX bug530: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU.
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發場景：
1. 一個執行緒持續分配物件並調用 `GcCell::borrow_mut()`
2. 另一個執行緒運行 lazy sweep reclaim 並 reuse slot
3. 在特定 timing 下，`has_gen_old` 會從新物件讀取，導致錯誤的 dirty page 記錄

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在小型物件路徑的 `has_gen_old` 讀取後、 `(header, index)` 返回前，新增第三個 `is_allocated` 檢查：

```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bug531: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU.
// Must verify slot is still allocated before returning to caller.
if !(*h).is_allocated(index) {
    return;
}
(header, index)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`gc_cell_validate_and_barrier`、`incremental_write_barrier` 和 `simple_write_barrier` 應該有一致的 TOCTOU 防護。缺少第三個檢查會導致在 slot reuse 場景下讀取到新物件的 `has_gen_old` flag，進而錯誤地將新物件的 page 記錄到 dirty pages。

**Rustacean (Soundness 觀點):**
在讀取跨 `GcBox` 的 field 後沒有再次驗證 slot 有效性，是 UAF 的潛在原因。即使使用了讀取後的 early return，仍然可能在 race 條件下讀取到已釋放物件的記憶體。

**Geohot (Exploit 觀點):**
如果攻擊者能控制 slot reuse 的時機，可以利用這個 bug 讓 GC 錯誤地追蹤記憶體位置，進一步利用記憶體佈局進行攻擊。