# [Bug]: Redundant Index Check in scan_page_for_marked_refs

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 每次呼叫都會發生 (程式碼總是執行) |
| **Severity (嚴重程度)** | Low | 效能問題，不影響正確性 |
| **Reproducibility (復現難度)** | N/A | 程式碼結構問題 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_marked_refs` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
掃描頁面時，應該只檢查一次物件是否已標記，避免多餘的計算。

### 實際行為 (Actual Behavior)
`scan_page_for_marked_refs` 函數對每個物件進行了兩次標記檢查：
1. 第一次使用迴圈索引 `i`: `!(*header).is_marked(i)`
2. 第二次使用計算出的索引 `idx`: `!(*header).is_marked(idx)`

這是冗餘的計算，會導致效能輕微下降。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/incremental.rs:766-776`：

```rust
for i in 0..obj_count {
    if (*header).is_allocated(i) && !(*header).is_marked(i) {  // 第一次檢查 (i)
        let obj_ptr = header.cast::<u8>().add(header_size + i * block_size);
        refs_found += 1;
        if let Some(idx) = crate::heap::ptr_to_object_index(obj_ptr.cast()) {
            if !(*header).is_marked(idx) {  // 第二次檢查 (idx) - 冗餘!
                (*header).set_mark(idx);
                // ...
            }
        }
    }
}
```

問題：
1. `i` 和 `idx` 應該總是相同（因為 `obj_ptr` 是根據 `i` 計算的）
2. 第一次檢查是無用的，因為如果需要 `idx` 來標記，那麼用 `i` 檢查就是多餘的
3. `ptr_to_object_index` 的調用也是浪費，因為我們已經知道物件的索引是 `i`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

這是一個程式碼結構問題，無法通过簡單的測試案例重現。可以通過效能分析工具（如 perf 或 cargo flamegraph）觀察每次 GC marking 時多餘的指標計算。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

簡化邏輯，移除冗餘檢查：

```rust
for i in 0..obj_count {
    if (*header).is_allocated(i) && !(*header).is_marked(i) {
        let obj_ptr = header.cast::<u8>().add(header_size + i * block_size);
        refs_found += 1;
        
        // 直接使用 i，不需要重新計算 idx
        (*header).set_mark(i);
        
        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
        if let Some(gc_box) = NonNull::new(gc_box_ptr as *mut GcBox<()>) {
            state.push_work(gc_box);
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
雖然這不是功能上的錯誤，但在效能敏感的 GC 路徑中，每次掃描頁面時浪費計算會累積。考慮到 GC 標記可能會掃描數百萬個物件，這種優化是值得的。

**Rustacean (Soundness 觀點):**
這是一個程式碼品質/效能問題，不影響 soundness。但重複檢查增加了代碼的複雜性和維護成本。

**Geohot (Exploit 觀點):**
從最佳化角度來看，這種冗餘在效能關鍵的路徑中是不可接受的。GC 暫停時間直接影響使用者體驗，特別是在即時應用中。
