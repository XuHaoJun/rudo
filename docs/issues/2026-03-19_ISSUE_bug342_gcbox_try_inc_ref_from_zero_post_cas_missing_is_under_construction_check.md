# [Bug]: GcBox::try_inc_ref_from_zero post-CAS 驗證缺少 is_under_construction 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Rare | 需要在 new_cyclic_weak 構造過程中並發調用 upgrade |
| **Severity (嚴重程度)** | High | 可能導致訪問未完成構造的物件 |
| **Reproducibility (復現難度)** | Low | 需精確時序，很難穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** GcBox::try_inc_ref_from_zero (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為
在 `try_inc_ref_from_zero` 的 post-CAS 驗證中，應該與 pre-CAS 檢查保持一致，同時檢查 `DEAD_FLAG`、`dropping_state()` 和 `is_under_construction()`。

### 實際行為
Post-CAS 驗證只檢查 `DEAD_FLAG` 和 `dropping_state()`，但**缺少** `is_under_construction()` 檢查。

```rust
// ptr.rs:315 (current code)
if (flags & Self::DEAD_FLAG) != 0 || self.dropping_state() != 0 {
    // rollback...
}
```

相比之下，pre-CAS 檢查（ptr.rs:296）有完整檢查：
```rust
if self.is_under_construction() {
    return false;
}
```

---

## 🔬 根本原因分析

在 `GcBox::try_inc_ref_from_zero` 函數中存在 TOCTOU 漏洞：

1. **Pre-CAS 檢查**（ptr.rs:294-298）：檢查 `is_under_construction()` 返回 false
2. **另一執行緒**設置 `UNDER_CONSTRUCTION_FLAG`（例如在 `Gc::new_cyclic_weak` 期間）
3. **CAS 成功**：ref_count 從 0 變為 1
4. **Post-CAS 驗證**（ptr.rs:309-321）：只檢查 `DEAD_FLAG` 和 `dropping_state()`，**跳過** `is_under_construction()` 檢查
5. **返回 true**：錯誤地允許复活正在構造中的物件

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// 需要精確時序：Gc::new_cyclic_weak 構造過程中並發調用 Weak::upgrade
// 理論上可觸發，但實際很難穩定重現
```

---

## 🛠️ 建議修復方案

在 post-CAS 驗證中添加 `is_under_construction()` 檢查：

```rust
// 修改 ptr.rs:315
if (flags & (Self::DEAD_FLAG | Self::UNDER_CONSTRUCTION_FLAG)) != 0 
    || self.dropping_state() != 0 
    || (weak_count_raw & Self::UNDER_CONSTRUCTION_FLAG) != 0  // 添加這行
{
    // rollback...
}
```

或者更簡潔地：
```rust
if (flags & (Self::DEAD_FLAG | Self::UNDER_CONSTRUCTION_FLAG)) != 0 
    || self.dropping_state() != 0 
{
    // rollback...
}
```

---

## 🗣️ 內部討論紀錄

**R. Kent Dybvig (GC 架構觀點):**
雖然 `UNDER_CONSTRUCTION_FLAG` 只在 `Gc::new_cyclic_weak` 期間設置，且時間窗口很小，但保持檢查的一致性是防御性編程的最佳實踐。GC 系統應該防止任何潛在的並發問題。

**Rustacean (Soundness 觀點):**
訪問未完成構造的物件是未定義行為 (UB)。即使窗口很小，也應該修復以確保內存安全。

**Geohot (Exploit 觀點):**
極端的時序攻擊可能利用這個窗口。雖然實際利用困難，但這是潛在的攻击面。

## Resolution (2026-03-21)

**Outcome:** Fixed.

In `ptr.rs`, the post-CAS verification in `try_inc_ref_from_zero` (the check introduced by bug287) only tested `DEAD_FLAG` and `dropping_state()`, missing `UNDER_CONSTRUCTION_FLAG`. Since `flags` already holds all flags from `weak_count & FLAGS_MASK`, the fix was to change:

```rust
if (flags & Self::DEAD_FLAG) != 0 || self.dropping_state() != 0 {
```

to:

```rust
if (flags & (Self::DEAD_FLAG | Self::UNDER_CONSTRUCTION_FLAG)) != 0
    || self.dropping_state() != 0
{
```

This closes the TOCTOU window where another thread could set `UNDER_CONSTRUCTION_FLAG` between the pre-CAS check and the CAS success, and makes post-CAS validation consistent with pre-CAS validation. Full test suite passes.
