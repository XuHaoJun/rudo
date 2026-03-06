# [Bug]: IncrementalMarkState::update_max_worklist_size TOCTOU Race Condition

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒並發呼叫 update_max_worklist_size |
| **Severity (嚴重程度)** | Medium | 導致 max_worklist_size 統計不準確，但不會造成記憶體錯誤 |
| **Reproducibility (復現難度)** | Medium | 需要並發 stress test 才能穩定觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `IncrementalMarkState::update_max_worklist_size` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`IncrementalMarkState::update_max_worklist_size` 函數存在經典的 TOCTOU (Time-Of-Check-Time-Of-Use) 競爭條件。

### 預期行為 (Expected Behavior)
在多執行緒並發環境下，`max_worklist_size` 應該正確記錄所有執行緒曾經看過的最大 worklist 大小。

### 實際行為 (Actual Behavior)
當多個執行緒同時呼叫 `update_max_worklist_size` 時，可能發生以下情況：
1. 執行緒 A 載入 current_max = 10
2. 執行緒 B 載入 current_max = 10
3. 執行緒 A 儲存 size = 20（因為 20 > 10）
4. 執行緒 B 儲存 size = 15（因為 15 > 10，但 A 已經儲存了更大的值）

結果：`max_worklist_size` 被錯誤地設為 15，而正確值應該是 20。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc/incremental.rs:413-418`：
```rust
#[inline]
fn update_max_worklist_size(&self, size: usize) {
    let current_max = self.max_worklist_size.load(Ordering::SeqCst);
    if size > current_max {
        self.max_worklist_size.store(size, Ordering::SeqCst);
    }
}
```

這段程式碼先載入 current_max，然後比較後再儲存。在載入和儲存之間，另一個執行緒可能已經更新了 max_worklist_size，導致 lost update。

此問題與 bug91 (`inc_weak`) 相同模式，後者已使用 `fetch_update` 修復。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒 stress test 才能穩定觸發
// 建議使用 ThreadSanitizer 或 stress test 環境驗證
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

參考 bug91 (`inc_weak`) 的修復方式，使用 `fetch_update`：

```rust
#[inline]
fn update_max_worklist_size(&self, size: usize) {
    self.max_worklist_size
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current_max| {
            if size > current_max {
                Some(size)
            } else {
                None
            }
        })
        .ok();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是並發統計資料收集的常見問題。在 parallel marking 環境中，多個 worker threads 同時更新 worklist大小，若不正确處理會導致統計不準確。雖然不會造成記憶體錯誤，但會影響 incremental marking 的決策（如何時觸發 STW fallback）。

**Rustacean (Soundness 觀點):**
這是純粹的並發 bug，不涉及記憶體安全或 UB。max_worklist_size 只是統計用途，不會導致 use-after-free 或其他記憶體錯誤。

**Geohot (Exploit 觀點):**
此 bug 不會造成可直接利用的記憶體錯誤。攻擊者無法利用統計不準確來達成任意記憶體讀寫。
