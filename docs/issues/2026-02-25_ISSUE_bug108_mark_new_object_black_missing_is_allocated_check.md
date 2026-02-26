# [Bug]: mark_new_object_black 缺少 is_allocated 檢查，與 mark_object_black 行為不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在特定並髮條件下觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致標記錯誤或潛在的 UAF |
| **Reproducibility (復現難度)** | Medium | 需要並髮場景才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_new_object_black`, `mark_object_black`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`mark_new_object_black` 函數在標記新分配的物件時，缺少對 `is_allocated` 的檢查。與同檔案中的 `mark_object_black` 函數行為不一致。

### 預期行為
- `mark_new_object_black` 和 `mark_object_black` 應該有一致的行為
- 兩個函數都應該檢查物件是否仍然有效配置 (`is_allocated`)

### 實際行為
- `mark_object_black` 有 `is_allocated` 檢查（註釋："Skip if object was swept; avoids UAF"）
- `mark_new_object_black` 沒有此檢查，直接標記物件

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc/incremental.rs` 中：

**mark_object_black (有檢查):**
```rust
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
        let header = crate::heap::ptr_to_page_header(ptr);
        let h = header.as_ptr();
        // Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
        if !(*h).is_allocated(idx) {  // <-- 有檢查
            return None;
        }
        if !(*h).is_marked(idx) {
            (*h).set_mark(idx);
            return Some(idx);
        }
    }
    ...
}
```

**mark_new_object_black (缺少檢查):**
```rust
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            if !(*header.as_ptr()).is_marked(idx) {  // <-- 缺少 is_allocated 檢查
                (*header.as_ptr()).set_mark(idx);
                return true;
            }
        }
    }
    false
}
```

問題：
1. 兩個函數用途類似，但安全檢查不一致
2. `mark_object_black` 的註釋明確說明檢查是為了避免 UAF
3. `mark_new_object_black` 缺少此防護，可能在極端情況下導致問題

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此問題可能需要並髮場景才能穩定重現。理論上的觸發條件：
1. 在增量標記期間
2. 一個 slot 被 sweep 後立即被重新分配
3. 新物件調用 `mark_new_object_black` 時可能存在 race condition

```rust
// 理論 PoC - 需要並髮標記才能穩定觸發
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::Mutex;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 觸發條件需要並髮 GC 和分配
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `mark_new_object_black` 中添加 `is_allocated` 檢查：

```rust
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            // 添加檢查以與 mark_object_black 保持一致
            if !(*header.as_ptr()).is_allocated(idx) {
                return false;
            }
            if !(*header.as_ptr()).is_marked(idx) {
                (*header.as_ptr()).set_mark(idx);
                return true;
            }
        }
    }
    false
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 增量標記期間，新分配的物件會被立即標記為黑色（black allocation優化）
- 這是 SATB 不變性的核心：標記期間分配的物件應被視為 live
- 問題是：如果 slot 在 sweep 後被重用，沒有 `is_allocated` 檢查可能導致標記過期的元數據

**Rustacean (Soundness 觀點):**
- 這是一個防禦性編程問題
- 雖然在當前實現中 allocation 和 sweep 不會並髮運行（STW），但代碼應該對未來可能的多執行緒場景保持安全
- 函數簽名也不一致：一個是 `unsafe fn`，另一個是 `fn`

**Geohot (Exploit 觀點):**
- 如果未來實現並髮 sweep，此處可能成為 UAF 的源頭
- 攻擊者可能嘗試在 slot 重用時干擾標記狀態
- 儘管目前難以利用，但這是一個潛在的攻擊面
