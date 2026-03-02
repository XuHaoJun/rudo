# [Bug]: Mark Scanning Functions TOCTOU - is_allocated 檢查與 set_mark 之间存在 race

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 在 concurrent GC 環境中，lazy sweep 與 marking 並發執行時很容易觸發 |
| **Severity (嚴重程度)** | High | 可能導致記憶體錯誤：標記已釋放物件、元件被錯誤標記為 live |
| **Reproducibility (復現難度)** | Medium | 需要並發場景，但可通過 stress test 穩定复現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `process_owned_page` (gc/marker.rs), `scan_page_for_marked_refs` (gc/incremental.rs), `scan_page_for_unmarked_refs` (gc/incremental.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

多個 mark scanning 函數存在 TOCTOU (Time-Of-Check-Time-Of-Use) race condition。這些函數使用非原子的「檢查然後標記」模式，而非正確的原子操作模式。

### 預期行為

使用 atomic 操作（如 `try_mark`）並在標記後重新檢查 `is_allocated`，類似 `mark_object_black` 的正確實現。

### 實際行為

函數使用 `is_allocated(i) && !is_marked(i)` 檢查，然後调用 `set_mark(i)`。在檢查和標記之間，另一執行緒可能 sweep 該物件，導致：

1. 標記已釋放的 slot
2. 將錯誤的物件推入 worklist
3. 可能導致 use-after-free 或記憶體錯誤

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 受影響的函數

1. **gc/marker.rs:687-701** - `process_owned_page`
```rust
for i in 0..obj_count {
    if unsafe { (*header).is_allocated(i) && !(*header).is_marked(i) } {  // CHECK
        // ...
        unsafe { (*header).set_mark(i) };  // USE - 非原子！
        self.push(gc_box_ptr.as_ptr());
    }
}
```

2. **gc/incremental.rs:804-817** - `scan_page_for_marked_refs`
```rust
for i in 0..obj_count {
    if (*header).is_allocated(i) && !(*header).is_marked(i) {  // CHECK
        let obj_ptr = header.cast::<u8>().add(header_size + i * block_size);
        refs_found += 1;
        (*header).set_mark(i);  // USE - 非原子！
        // ...
    }
}
```

3. **gc/incremental.rs:913-926** - `scan_page_for_unmarked_refs`
```rust
for i in 0..obj_count {
    if (*header).is_allocated(i) && !(*header).is_marked(i) {  // CHECK
        let obj_ptr = header.cast::<u8>().add(header_size + i * block_size);
        if (*header).set_mark(i) {  // USE - 部分原子但缺少事後檢查
            // ...
        }
    }
}
```

### 正確模式（已存在於 codebase）

`mark_object_black` (gc/incremental.rs:978-1003) 使用正確模式：
```rust
// 1. 檢查 allocation
if !(*h).is_allocated(idx) {
    return None;
}

// 2. 原子標記
loop {
    match (*h).try_mark(idx) {
        Ok(false) => return Some(idx), // 已經標記
        Ok(true) => {
            // 3. 標記後重新檢查 allocation
            if (*h).is_allocated(idx) {
                return Some(idx);
            }
            // Slot 在檢查和標記之間被 sweep，回滾
            (*h).clear_mark_atomic(idx);
            return None;
        }
        Err(()) => {} // CAS 失敗，重試
    }
}
```

### Race 條件說明

1. Thread A 進入 scanning 函數，檢查 `is_allocated(i)` 返回 true
2. Thread B 完成 lazy sweep，標記 slot 為 unallocated
3. Thread A 調用 `set_mark(i)` - 標記了一個已釋放的 slot
4. Thread A 將錯誤的指標推入 worklist
5. 後續處理可能導致 UAF 或記憶體損壞

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發 stress test：
1. 多執行緒同時分配物件
2. 一個執行緒執行 minor/major GC (觸發 marking)
3. 另一個執行緒執行 lazy sweep
4. 反覆執行導致 race 窗口被觸發

```rust
// 概念驗證（需要 Miri 或 ThreadSanitizer）
#[test]
fn test_mark_scan_toctou() {
    // 1. 分配多個物件
    // 2. 啟動多個 GC worker threads
    // 3. 同時觸發 sweep
    // 4. 驗證是否有錯誤的標記或 UAF
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將受影響的函數改用與 `mark_object_black` 相同的模式：

1. **使用 `try_mark(idx)`** 代替 `set_mark(i)` - 原子 CAS 操作
2. **在成功標記後重新檢查 `is_allocated(idx)`** - 檢測 sweep race
3. **如果 slot 被釋放，回滾標記** - `clear_mark_atomic(idx)`

或者，使用更簡單的策略：
- 在標記前捕獲所有指標（樂觀標記）
- 依賴 `mark_object_black` 的內建檢查（它已經是安全的）

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**

在 Chez Scheme 的 GC 實現中，我們使用類似的頁面掃描優化。關鍵是確保標記操作是原子的，或者在標記後驗證物件仍然有效。本質上，這是一個經典的「檢查-使用」問題，需要將檢查和標記合併為原子操作。

**Rustacean (Soundness 觀點):**

這不是傳統意義上的 UB，但可能導致記憶體不安全。`set_mark` 訪問的 page header 可能屬於已釋放的記憶體。如果該 page 被重新分配，可能導致資料損壞。正確使用 atomic 操作的修復方案是 sound 的。

**Geohot (Exploit 觀點):**

在高負載 GC 環境中，這個 race 窗口是可利用的。攻擊者可以：
1. 噴射大量物件
2. 精確控制 GC 時序
3. 誘使 mark scan 標記已釋放的物件
4. 導致 use-after-free 來劫持控制流

修復方案簡單且高效：使用現有的 `try_mark` + recheck 模式即可。
