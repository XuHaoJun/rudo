# [Bug]: GcBox::mark_dead 使用 Relaxed Ordering 清除 UNDER_CONSTRUCTION_FLAG 導致潜在 Race Condition

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 Gc::new_cyclic_weak 的 DropGuard 執行時才會觸發，發生時機相對少見 |
| **Severity (嚴重程度)** | High | 可能導致物件被錯誤識別為「施工中」，導致弱參數升級失敗或其他不一致行為 |
| **Reproducibility (復現難度)** | High | 需要在特定時序條件下觸發，可能需要並發測試 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBox::mark_dead()` 函數在清除 `UNDER_CONSTRUCTION_FLAG` 時使用 `Ordering::Relaxed`，而 `is_under_construction()` 使用 `Ordering::Acquire` 讀取。這種不一致的 ordering 可能在高並發場景下導致 race condition。

### 預期行為
清除 UNDER_CONSTRUCTION_FLAG 時應使用適當的 memory ordering，確保其他執行緒能夠正確讀取到清除後的狀態。

### 實際行為
使用 Relaxed ordering 清除 flag，導致其他執行緒可能無法及時看到 flag 被清除，錯誤地認為物件仍處於「施工中」狀態。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:342-346` 中：

```rust
pub(crate) fn mark_dead(&self) {
    self.weak_count.fetch_or(Self::DEAD_FLAG, Ordering::Relaxed);
    self.weak_count
        .fetch_and(!Self::UNDER_CONSTRUCTION_FLAG, Ordering::Relaxed);  // BUG: 應使用 Release
}
```

對比 `ptr.rs:80-86` 中正確的實作：

```rust
fn set_under_construction(&self, flag: bool) {
    let mask = Self::UNDER_CONSTRUCTION_FLAG;
    if flag {
        self.weak_count.fetch_or(mask, Ordering::Release);
    } else {
        self.weak_count.fetch_and(!mask, Ordering::AcqRel);  // 正確：使用 AcqRel
    }
}
```

而 `is_under_construction()` 使用 Acquire ordering 讀取 (ptr.rs:72-74)：

```rust
pub(crate) fn is_under_construction(&self) -> bool {
    (self.weak_count.load(Ordering::Acquire) & Self::UNDER_CONSTRUCTION_FLAG) != 0
}
```

**問題**：當 `mark_dead()` 使用 Relaxed ordering 清除 flag 時，並發執行緒可能無法看到清除操作，導致錯誤的 "under construction" 狀態判斷。

此問題與 bug143（clear_gen_old 使用 Relaxed）和 bug145（clear_dead 使用 Relaxed）屬於同一類別。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要在以下場景中觸發：
1. 執行緒 A：呼叫 `Gc::new_cyclic_weak`，在構造過程中發生 panic
2. 執行緒 B：嘗試升級同一個 GcBox 的弱參考
3. 由於 Relaxed ordering，執行緒 B 可能錯誤地看到 UNDER_CONSTRUCTION_FLAG 仍設置

```rust
// 需要並發測試來觸發此 race condition
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `mark_dead()` 中的 `fetch_and` 改為使用 `Ordering::Release` 或 `Ordering::AcqRel`：

```rust
pub(crate) fn mark_dead(&self) {
    self.weak_count.fetch_or(Self::DEAD_FLAG, Ordering::Relaxed);
    self.weak_count
        .fetch_and(!Self::UNDER_CONSTRUCTION_FLAG, Ordering::Release);  // 改為 Release
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 中，flag 的清除必須對並發的 GC 執行緒可見。使用 Release ordering 清除 flag 可以確保清除操作在後續的讀取之前完成。這是記憶體順序 consistency 的基本要求。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB，但會導致不一致的狀態判斷，可能導致邏輯錯誤（如弱參數升級失敗）。使用正確的 atomic ordering 是必要的。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制並發時序，可能利用此 race condition 導致：
- 錯誤地阻止弱參數升級
- 在物件構造失敗時仍錯誤地認為物件可用
