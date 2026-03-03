# [Bug]: GcBox::dec_ref 缺少 is_under_construction 檢查 - 可能導致提前釋放構造中的物件

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 目前調用者多數有保護機制，但缺少防御性檢查 |
| **Severity (嚴重程度)** | High | 可能導致 UAF 或提前釋放未完全構造的物件 |
| **Reproducibility (復現難度)** | Medium | 需要構造特定時序或並發場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::dec_ref()`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (GcBox::decExpected Behavior)

`_ref()` 應該在遞減引用計數前檢查物件是否正在構造中 (`is_under_construction()`)，與其他類似函數（如 `Weak::upgrade()`, `Gc::clone()`, `Gc::downgrade()` 等）保持一致的行為。

### 實際行為 (Actual Behavior)

`dec_ref` 函數檢查了：
- `dead_flag` (line 155-161)
- `ref_count == 0` (line 163-166)
- `dropping_state` (line 167)

但**沒有**檢查 `is_under_construction()`。

### 程式碼位置

`ptr.rs` 第 151-192 行 (`GcBox::dec_ref` 函數)：
```rust
pub fn dec_ref(self_ptr: *mut Self) -> bool {
    // SAFETY: self_ptr is valid because it's obtained from the atomic pointer in Gc::drop
    let this = unsafe { &*self_ptr };
    loop {
        let dead_flag = this.weak_count_raw() & GcBox::<()>::DEAD_FLAG;
        if dead_flag != 0 {
            return false;
        }

        let count = this.ref_count.load(Ordering::Acquire);
        if count == 0 {
            return false;
        }
        if count == 1 && this.dropping_state() == 0 {
            // ... drop logic
        }
        // 缺少 is_under_construction() 檢查！
    }
}
```

### 對比：其他函數的正確實現

其他類似函數都檢查了 `is_under_construction()`：
- `Gc::clone()` (ptr.rs:475): `if gc_box.is_under_construction()`
- `Gc::downgrade()` (ptr.rs:609): `if gc_box.is_under_construction()`
- `Weak::upgrade()` (ptr.rs:1847): `if gc_box.is_under_construction()`
- `GcHandle::resolve()` (ptr.rs:1419): `if gc_box.is_under_construction()`

---

## 🔬 根本原因分析 (Root Cause Analysis)

`dec_ref` 是用於遞減 GC 物件引用計數的核心函數。當物件正在構造中時：
1. `ref_count` 初始化為 1
2. `weak_count` 包含 `UNDER_CONSTRUCTION_FLAG`

如果在構造過程中錯誤地調用了 `dec_ref`（例如，通過某個未預料到的 code path），它會看到 `ref_count == 1` 並執行 `drop_fn`，導致：
- 提前釋放尚未完全構造的物件
- 可能的 use-after-free
- 記憶體不安全

雖然大多數正常的調用路徑都有保護機制（`Gc::new` 在返回前會設置 `set_under_construction(false)`），但缺乏防御性檢查是一個潛在的安全隱患。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上的 PoC（需要找到實際的觸發路徑）：

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// 需要找到能夠在 is_under_construction == true 時調用 dec_ref 的場景
// 例如：並發創建 Gc 時的 race condition
```

**注意**：此 bug 可能難以穩定重現，因為正常情況下 `Gc::new` 會確保物件完全構造後才返回。但根據代码中其他類似函數都添加了此檢查（如 bug89, bug92, bug94, bug100, bug182 等），這是一個防御性編碼的最佳實踐。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBox::dec_ref()` 函數中添加 `is_under_construction()` 檢查：

```rust
pub fn dec_ref(self_ptr: *mut Self) -> bool {
    let this = unsafe { &*self_ptr };
    loop {
        let dead_flag = this.weak_count_raw() & GcBox::<()>::DEAD_FLAG;
        if dead_flag != 0 {
            return false;
        }

        // 新增：檢查是否正在構造中
        if this.is_under_construction() {
            // 物件正在構造中，不應遞減引用計數
            return false;
        }

        let count = this.ref_count.load(Ordering::Acquire);
        if count == 0 {
            return false;
        }
        // ... 其餘邏輯保持不變
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個防御性編碼的問題。雖然正常的物件創建流程有保護機制，但在 GC 實現中，確保所有操作在物件狀態不明確時能夠安全地失敗是重要的。添加 `is_under_construction` 檢查可以防止潛在的 race condition。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 UB（因為正常調用路徑有保護），但缺少檢查可能導致在邊界情況下出現記憶體不安全。添加此檢查符合「fail fast」的防御性編碼原則。

**Geohot (Exploit 攻擊觀點):**
如果存在某個 code path 可以讓攻擊者控制時序，在物件構造過程中觸發 `dec_ref`，則可能導致 UAF。但目前來看，正常調用路徑難以觸發此問題。
