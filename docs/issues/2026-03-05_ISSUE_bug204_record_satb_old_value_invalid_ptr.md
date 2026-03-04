# [Bug]: record_satb_old_value 記錄已釋放物件 - 當 allocating_thread_id 為 0 時仍推送指標

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Low` | 需要在 GC 指針被覆蓋前對象已被釋放的極端情況 |
| **Severity (嚴重程度)** | `Medium` | 可能導致 GC 嘗試追蹤無效記憶體，但不會造成 UAF |
| **Reproducibility (復現難度)** | `Very High` | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::record_satb_old_value()` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`record_satb_old_value()` 應該只記錄有效的 GC 指標。當對象已被釋放（`allocating_thread_id == 0`，表示地址在 heap 範圍之外）時，不應該記錄該指標。

### 實際行為 (Actual Behavior)
當 `allocating_thread_id == 0` 時（表示對象不在 heap 中或已被釋放），代碼仍然將指標推入本地 SATB buffer (`self.satb_old_values.push(gc_box)`)。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs` 的 `record_satb_old_value` 函數 (lines 1924-1939)：

```rust
pub fn record_satb_old_value(&mut self, gc_box: NonNull<GcBox<()>>) -> bool {
    let current_thread_id = get_thread_id();
    let allocating_thread_id = unsafe { get_allocating_thread_id(gc_box.as_ptr() as usize) };

    // Bug: 當 allocating_thread_id == 0 時，仍推入本地 buffer
    if current_thread_id != allocating_thread_id && allocating_thread_id != 0 {
        Self::push_cross_thread_satb(gc_box);
        return true;
    }

    // 這裡沒有檢查 allocating_thread_id 是否為 0！
    self.satb_old_values.push(gc_box);
    // ...
}
```

`get_allocating_thread_id` 返回 0 的情況：
1. 對象地址在 heap 範圍之外
2. 對象已被釋放（deallocated）

當 `allocating_thread_id == 0` 時，不應該記錄該指標，因為：
1. 該對象已無效
2. 在 SATB 中記錄無效指標會導致 GC 嘗試追蹤無效記憶體

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要精確的時序控制，單執行緒難以重現。概念上：

1. 創建一個 GC 對象
2. 獲取其地址並通過某種方式使地址變得無效（例如，通過释放页面）
3. 在 GcCell 中覆蓋 GC 指針
4. 觀察 record_satb_old_value 是否記錄了無效指標

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在將指標推入本地 SATB buffer 之前，添加對 `allocating_thread_id` 的檢查：

```rust
pub fn record_satb_old_value(&mut self, gc_box: NonNull<GcBox<()>>) -> bool {
    let current_thread_id = get_thread_id();
    let allocating_thread_id = unsafe { get_allocating_thread_id(gc_box.as_ptr() as usize) };

    if allocating_thread_id == 0 {
        // 對象無效，不記錄
        return true;
    }

    if current_thread_id != allocating_thread_id {
        Self::push_cross_thread_satb(gc_box);
        return true;
    }

    self.satb_old_values.push(gc_box);
    // ...
}
```

或者，更保守的方法是返回 `true` 而不記錄，讓調用者處理這種邊界情況。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB barrier 的目的是保留在標記開始時可達的對象。記錄無效/已釋放的對象沒有任何好處，反而可能導致 GC 行為異常。這個檢查是必要的防禦性編程。

**Rustacean (Soundness 觀點):**
雖然記錄無效指標可能不會直接導致 UAF（因為 GC 會檢查對象有效性），但這是一個防禦性編程問題。記錄無效指標會浪費內存，並可能導致不確定的 GC 行為。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個邊界情況來觸發異常的 GC 行為。雖然實際利用難度較高，但這是一個潛在的攻擊面。

---

## 驗證記錄 (Verification Record)

**驗證日期:** 2026-03-05
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `crates/rudo-gc/src/heap.rs` 的 `record_satb_old_value` 函數：

1. `get_allocating_thread_id` 對無效地址返回 0 (heap.rs:75-77)
2. 當 `allocating_thread_id == 0` 時，跨執行緒分支被跳過 (line 1928)
3. 但本地記錄分支 (line 1933) 沒有進行相同的檢查
4. 這導致無效指標被錯誤地推入 SATB buffer

**結論:** Bug 確認存在，需要修復以確保只記錄有效的 GC 指標。
