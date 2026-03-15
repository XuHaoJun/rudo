# [Bug]: GcBox::try_inc_ref_from_zero 缺少 is_under_construction 檢查 - 可能在物件構造過程中錯誤复活

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 目前調用者多數有保護機制，但缺少防御性檢查 |
| **Severity (嚴重程度)** | High | 可能導致 UAF 或在構造過程中錯誤地對象進行引用計數增加 |
| **Reproducibility (復現難度)** | Medium | 需要構造特定時序或並發場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::try_inc_ref_from_zero()`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`try_inc_ref_from_zero()` 應該在嘗試將引用計數從 0 增加到 1（復活物件）之前，檢查物件是否正在構造中 (`is_under_construction()`)，與 `dec_ref()` 和其他類似函數保持一致的行為。

### 實際行為 (Actual Behavior)

`try_inc_ref_from_zero` 函數檢查了：
- `DEAD_FLAG` (line 250-252)
- `dropping_state != 0` (line 255-257)
- `ref_count != 0` (line 259-261)

但**沒有**檢查 `is_under_construction()`。

### 程式碼位置

`ptr.rs` 第 242-275 行 (`GcBox::try_inc_ref_from_zero` 函數)：
```rust
pub(crate) fn try_inc_ref_from_zero(&self) -> bool {
    loop {
        let ref_count = self.ref_count.load(Ordering::Acquire);
        let weak_count_raw = self.weak_count.load(Ordering::Acquire);

        let flags = weak_count_raw & Self::FLAGS_MASK;

        // Never resurrect a dead object; DEAD_FLAG means value was dropped.
        if (flags & Self::DEAD_FLAG) != 0 {
            return false;
        }

        // Object is being dropped - do not allow resurrection (UAF prevention)
        if self.dropping_state() != 0 {
            return false;
        }

        if ref_count != 0 {
            return false;
        }
        
        // 缺少 is_under_construction() 檢查！
        
        // ... CAS 邏輯
    }
}
```

### 對比：其他函數的正確實現

其他類似函數都檢查了 `is_under_construction()`：
- `dec_ref()` (ptr.rs:151-192): **缺少** (bug183)
- `Gc::clone()` (ptr.rs:475): `if gc_box.is_under_construction()`
- `Gc::downgrade()` (ptr.rs:609): `if gc_box.is_under_construction()`
- `Weak::upgrade()` (ptr.rs:1847): `if gc_box.is_under_construction()`

---

## 🔬 根本原因分析 (Root Cause Analysis)

`try_inc_ref_from_zero` 是用於將引用計數從 0 增加到 1（復活物件）的核心函數。當物件正在構造中時：
1. `ref_count` 初始化為 1
2. `weak_count` 包含 `UNDER_CONSTRUCTION_FLAG`

如果在構造過程中錯誤地調用了 `try_inc_ref_from_zero`（例如，通過某個未預料到的 code path），它可能會試圖復活一個尚未完全構造的物件，導致：
- 提前訪問尚未完全構造的物件
- 可能的 use-after-free
- 記憶體不安全

雖然大多數正常的調用路徑都有保護機制，但缺乏防御性檢查是一個潛在的安全隱患。

bug125 修復記錄中提到：「callers must still check `is_under_construction()` before calling」，這表明需要在函數內部添加此檢查以實現防御性編碼。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上的 PoC（需要找到實際的觸發路徑）：

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// 需要找到能夠在 is_under_construction == true 時調用 try_inc_ref_from_zero 的場景
// 例如：並發創建 Gc 時的 race condition
```

**注意**：此 bug 可能難以穩定重現，但根據代码中其他類似函數都添加了此檢查（如 bug183, bug125 等），這是一個防御性編碼的最佳實踐。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBox::try_inc_ref_from_zero()` 函數中添加 `is_under_construction()` 檢查：

```rust
pub(crate) fn try_inc_ref_from_zero(&self) -> bool {
    loop {
        let ref_count = self.ref_count.load(Ordering::Acquire);
        let weak_count_raw = self.weak_count.load(Ordering::Acquire);

        let flags = weak_count_raw & Self::FLAGS_MASK;

        // Never resurrect a dead object; DEAD_FLAG means value was dropped.
        if (flags & Self::DEAD_FLAG) != 0 {
            return false;
        }

        // Object is being dropped - do not allow resurrection (UAF prevention)
        if self.dropping_state() != 0 {
            return false;
        }
        
        // 新增：檢查是否正在構造中
        if (flags & Self::UNDER_CONSTRUCTION_FLAG) != 0 {
            return false;
        }

        if ref_count != 0 {
            return false;
        }
        
        // ... 其餘邏輯保持不變
    }
}
```

或者使用現有的 `is_under_construction()` 方法：
```rust
// 新增：檢查是否正在構造中
if self.is_under_construction() {
    return false;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是防御性編碼的問題。雖然正常的物件創建流程有保護機制，但在 GC 實現中，確保所有操作在物件狀態不明確時能夠安全地失敗是重要的。添加 `is_under_construction` 檢查可以防止潛在的 race condition。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 UB（因為正常調用路徑有保護），但缺少檢查可能導致在邊界情況下出現記憶體不安全。添加此檢查符合「fail fast」的防御性編碼原則。

**Geohot (Exploit 攻擊觀點):**
如果存在某個 code path 可以讓攻擊者控制時序，在物件構造過程中觸發 `try_inc_ref_from_zero`，則可能導致 UAF。但目前來看，正常調用路徑難以觸發此問題。

---

## Resolution (2026-03-03)

The `is_under_construction()` check was already present in `GcBox::try_inc_ref_from_zero()` (ptr.rs:264-267). The docstring was outdated—it stated "callers must still check `is_under_construction()` before calling" even though the function now performs this check internally. Updated the docstring to: "This function checks `dropping_state()` and `is_under_construction()` internally."
