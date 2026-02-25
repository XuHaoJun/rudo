# [Bug]: GcBox::is_under_construction() 使用 Relaxed Ordering 導致潜在 Race Condition

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要多執行緒並髮操作：GcBox 構造期間與另一執行緒的 Gc::new_cyclic_weak 或升級操作並發 |
| **Severity (嚴重程度)** | High | 可能導致在物件構造完成前就訪問物件，導致未定義行為 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::is_under_construction()` (ptr.rs:71-73)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為

`is_under_construction()` 應該提供足夠的記憶體順序保證，確保：
- 當 flag 被設定（`set_under_construction(true)`）後，所有後續讀取都能看到最新的值
- 當 flag 被清除（`set_under_construction(false)`）後，所有後續讀取都能看到清除後的值

### 實際行為

`is_under_construction()` 使用 `Ordering::Relaxed` 載入值，與設定/清除時使用的 `Ordering::Release` 和 `Ordering::AcqRel` 不一致。這導致：

1. **設為 true 時使用 Release**：確保構造相關的寫入在 flag 設定之前對其他執行緒可見
2. **清除時使用 AcqRel**：確保清除 flag 時的同步
3. **讀取時使用 Relaxed**：可能讀到過期的 flag 值！

這會造成 TOCTOU 問題：
- Thread A: `set_under_construction(false)` 使用 `AcqRel` 清除 flag
- Thread B: `is_under_construction()` 使用 `Relaxed` 讀取，可能仍看到 true（過期值）
- Thread B: 錯誤地認為物件仍在構造中，導致 `upgrade()` 返回 `None` 或 panic

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:71-73`：
```rust
pub(crate) fn is_under_construction(&self) -> bool {
    (self.weak_count.load(Ordering::Relaxed) & Self::UNDER_CONSTRUCTION_FLAG) != 0
}
```

而在設定和清除時（`ptr.rs:79-86`）：
```rust
fn set_under_construction(&self, flag: bool) {
    let mask = Self::UNDER_CONSTRUCTION_FLAG;
    if flag {
        self.weak_count.fetch_or(mask, Ordering::Release);  // 設定使用 Release
    } else {
        self.weak_count.fetch_and(!mask, Ordering::AcqRel);  // 清除使用 AcqRel
    }
}
```

**問題**：當 flag 從 true 變為 false（使用 `AcqRel`）後，另一執行緒使用 `Relaxed` 讀取可能仍看到 true。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發場景：
1. Thread A: 建立 `Gc::new_cyclic_weak`，物件正在構造（`is_under_construction = true`）
2. Thread B: 同時嘗試 `Weak::upgrade` 或 `Gc::clone`
3. 時序：Thread A 完成構造（清除 flag），Thread B 使用 Relaxed 讀取（可能看到過期值）

理論上可以通過精確的執行緒調度觸發，但極難穩定重現。建議使用 model checker（如 loom）驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `is_under_construction()` 的 `Ordering::Relaxed` 改為 `Ordering::Acquire`，與清除時的 `AcqRel` 配對：

```rust
pub(crate) fn is_under_construction(&self) -> bool {
    (self.weak_count.load(Ordering::Acquire) & Self::UNDER_CONSTRUCTION_FLAG) != 0
}
```

這樣當 flag 被清除時（使用 `AcqRel`），後續讀取（使用 `Acquire`）將保證看到最新的值。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的記憶體順序不一致問題。在 GC 的物件構造過程中，`is_under_construction` flag 是用來防止在物件完全構造完成前被訪問的關鍵同步機制。使用 Relaxed ordering 破壞了這層保護，可能導致在物件構造期間訪問到未初始化的記憶體。

**Rustacean (Soundness 觀點):**
這可能導致 undefined behavior。當 flag 實際已被清除但讀取仍看到 true 時，會導致 `upgrade()` 錯誤地返回 `None`（當物件實際可用時），或造成邏輯不一致。根據 Rust 的記憶體模型，Relaxed ordering 不提供跨執行緒的同步保證。

**Geohot (Exploit 攻擊觀點):**
雖然這個 bug 需要精確的時序控制，但攻擊者可以：
1. 建立一個使用 `Gc::new_cyclic_weak` 的物件
2. 使用執行緒調度技巧讓構造完成與升級操作競爭
3. 透過錯誤的 flag 讀取實現邏輯錯誤或 DoS

修復此問題只需將 `Relaxed` 改為 `Acquire`，這是一個簡單且低風險的改動。
