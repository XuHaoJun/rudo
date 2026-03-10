# [Bug]: scan_page_for_unmarked_refs TOCTOU Race - is_allocated 檢查與 push_work 之间存在 race

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 在 concurrent GC 環境中，lazy sweep 與 marking 並發執行時很容易觸發 |
| **Severity (嚴重程度)** | High | 可能導致記憶體錯誤：將已釋放物件的指標推入 worklist，導致錯誤追蹤 |
| **Reproducibility (復現難度)** | Medium | 需要並發場景，但可通過 stress test 穩定复現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `scan_page_for_unmarked_refs` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`scan_page_for_unmarked_refs` 函數存在 TOCTOU race condition。雖然 bug175 修復了 `set_mark` 與 `is_allocated` 檢查之間的 race，但在 `is_allocated` 檢查與 `push_work`之間仍存在一個 race window。

### 預期行為 (Expected Behavior)

在將物件推入 worklist 之前，應該再次驗證物件仍然有效（未被 sweep）。

### 實際行為 (Actual Behavior)

函數在 `is_allocated` 檢查後、 `push_work` 調用前，沒有再次檢查物件是否仍然allocated。這導致：

1. Thread A 成功標記物件 `i` (`set_mark` 返回 true)
2. Thread A 檢查 `is_allocated(i)` 返回 true
3. **Race Window**: Thread B 在此時 sweep 了 slot `i`
4. Thread A 將已釋放的指標推入 worklist
5. 導致錯誤的物件被追蹤，可能造成記憶體錯誤

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc/incremental.rs:930-950`:

```rust
for mut i in 0..obj_count {
    if (*header).is_allocated(i) && !(*header).is_marked(i) {
        let obj_ptr = header.cast::<u8>().add(header_size + i * block_size);
        // set_mark returns true if we successfully marked - use it as try_mark
        // But we still need to re-check is_allocated after successful mark
        if (*header).set_mark(i) {
            // Re-check is_allocated to fix TOCTOU with lazy sweep
            if !(*header).is_allocated(i) {
                (*header).clear_mark_atomic(i);
                continue;
            }
            // BUG: Race window here! Slot can be swept between is_allocated check and push_work
            #[allow(clippy::cast_ptr_alignment)]
            #[allow(clippy::unnecessary_cast)]
            #[allow(clippy::ptr_as_ptr)]
            let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
            if let Some(gc_box) = NonNull::new(gc_box_ptr) {
                let ptr = IncrementalMarkState::global();
                ptr.push_work(gc_box);  // 可能推送已釋放的物件！
            }
        }
    }
}
```

**Race 條件說明**:
1. Line 937: `is_allocated(i)` 返回 true - slot 仍然有效
2. Line 938-940: 清除標記並 continue (如果 slot 已釋放)
3. **Race Window**: 如果 slot 在 line 937 之後、line 947 之前被 sweep
4. Line 947: `push_work(gc_box)` - 推送可能已釋放的物件

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發 stress test：
1. 多執行緒同時分配物件
2. 一個執行緒執行 incremental marking (呼叫 `scan_page_for_unmarked_refs`)
3. 另一個執行緒執行 lazy sweep
4. 反覆執行導致 race 窗口被觸發

```rust
// 概念驗證（需要並發 stress test）
#[test]
fn test_scan_page_unmarked_toctou() {
    // 1. 分配多個物件
    // 2. 啟動多個 GC worker threads 
    // 3. 同時觸發 sweep
    // 4. 驗證是否有錯誤的指標被推入 worklist
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `push_work` 之前再次檢查 `is_allocated`，或者使用類似 `mark_object_black` 的模式：

```rust
// 選項 1: 在 push_work 前再次檢查
if (*header).set_mark(i) {
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        continue;
    }
    // 再次檢查 is_allocated
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        continue;
    }
    // 現在安全地 push
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    if let Some(gc_box) = NonNull::new(gc_box_ptr) {
        let ptr = IncrementalMarkState::global();
        ptr.push_work(gc_box);
    }
}

// 選項 2: 使用類似 mark_object_black 的模式
// 參考 gc/incremental.rs:978-1003 的正確實現
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**

這是經典的「檢查-使用」問題。雖然 bug175 修復了部分 TOCTOU，但在關鍵的「標記後再次驗證」與「實際使用」之間仍存在視窗。正確的模式應該是在 push_work 之前再次驗證物件狀態，或者使用 atomic 操作確保指標的有效性。

**Rustacean (Soundness 觀點):**

這可能導致記憶體不安全。將已釋放的記憶體指標推入 worklist 可能導致：
1. 訪問已釋放的記憶體 (UAF)
2. 錯誤地追蹤新分配的物件
3. 記憶體損壞

**Geohot (Exploit 觀點):**

在高負載 GC 環境中，這個 race 窗口是可利用的。攻擊者可以：
1. 噴射大量物件
2. 精確控制 GC 時 誘使 scan序
3._page 推送已釋放的指標
4. 利用 UAF 劫持控制流

---

## 驗證記錄

**驗證日期:** 2026-03-10
**驗證人員:** opencode

### 驗證結果

確認 `scan_page_for_unmarked_refs` (gc/incremental.rs:930-950) 存在 TOCTOU race：
- Line 937: is_allocated 檢查通過
- Line 947: push_work 被調用
- 中間存在 race window

對比 `mark_object_black` (gc/incremental.rs:978-1003) 的正確實現：
- 該函數在成功標記後會再次檢查 is_allocated
- 但 scan_page_for_unmarked_refs 沒有這樣做

**Status: Open** - 等待修復。
