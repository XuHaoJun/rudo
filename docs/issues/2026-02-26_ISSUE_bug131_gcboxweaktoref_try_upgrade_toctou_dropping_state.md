# [Bug]: GcBoxWeakRef::try_upgrade TOCTOU - caller's dropping_state check 與 try_inc_ref_from_zero 之間的 Race

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：try_upgrade 執行時物件正在被 drop |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::try_upgrade()`, `ptr.rs:598-620`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當物件正在被 drop（`dropping_state != 0`）時，`try_upgrade()` 應返回 `None`。

### 實際行為 (Actual Behavior)

1. Thread A 調用 `try_upgrade()`，通過初始檢查（is_under_construction, is_dead_or_unrooted, dropping_state）
2. Thread A 檢查 `dropping_state() != 0`（line 598）- 通過
3. Thread B 開始 drop 物件：設置 `dropping_state = 1`
4. Thread A 調用 `try_inc_ref_from_zero()`（line 603）- 此函數不檢查 dropping_state！
5. Thread A 可能成功遞增 ref_count，返回對正在被 drop 的物件的 Gc！

**問題根源：**
- Line 598 檢查 `dropping_state()`
- Line 603 調用 `try_inc_ref_from_zero()`
- 但 `try_inc_ref_from_zero()` 內部**不檢查 dropping_state**（這是 bug125 的問題）
- 在檢查和調用之間存在 TOCTOU race window

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:598-620`：

```rust
// Line 598: 檢查 dropping_state
if gc_box.dropping_state() != 0 {
    return None;
}

// Line 603: 調用 try_inc_ref_from_zero - 但之間沒有任何同步！
if gc_box.try_inc_ref_from_zero() {
    // ...
}
```

問題：
1. 檢查 `dropping_state()` 和調用 `try_inc_ref_from_zero()` 是分離的，非原子操作
2. `try_inc_ref_from_zero()` 內部不檢查 `dropping_state()`（bug125）
3. 在多執行緒環境下，另一執行緒可以在檢查和調用之間設置 `dropping_state`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 理論上的攻擊序列：
// 1. 建立一個 Gc 物件和其 GcBoxWeakRef
// 2. Thread A 調用 try_upgrade()，通過 is_under_construction, is_dead_or_unrooted 檢查
// 3. Thread A 到達 dropping_state() 檢查 - 此時為 0
// 4. Thread B 開始 drop 物件，設置 dropping_state = 1
// 5. Thread A 調用 try_inc_ref_from_zero() - 不檢查 dropping_state！
// 6. Thread A 返回 Gc 到正在被 drop 的物件！

// 建議使用 model checker（如 loom）驗證。
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：在 try_upgrade 中第二次檢查 dropping_state（在 try_inc_ref_from_zero 之後）
```rust
// Try atomic transition from 0 to 1 (same as regular upgrade)
if gc_box.try_inc_ref_from_zero() {
    // Second check: verify object wasn't dropped between check and CAS
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        // Undo the increment and return None
        let _ = gc_box;
        crate::ptr::GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
    // ...
}
```

選項 2：在 try_inc_ref_from_zero 內部添加 dropping_state 檢查（這會同時修復 bug125）

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 RC upgrade TOCTOU 問題。調用者的檢查和實際操作之間必須原子化，或者在操作後再次驗證物件狀態。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當物件正在被 drop 時（dropping_state != 0），不應允許遞增引用計數。

**Geohot (Exploit 攻擊觀點):**
雖然需要精確的時序控制，但理論上可以通過構造並發場景來觸發此 bug，導致 use-after-free。

---

## 備註

此問題與 bug125 相關：
- bug125: try_inc_ref_from_zero 內部缺少 dropping_state 檢查
- 本 bug: try_upgrade 中調用者的 dropping_state 檢查與 try_inc_ref_from_zero 之間的 TOCTOU

本 bug 是 bug125 的另一個表現形式 - 一個是 API 設計問題，一個是使用上的 TOCTOU。

---

## Resolution (2026-02-27)

**Outcome:** Fixed (already addressed in codebase).

`GcBoxWeakRef::try_upgrade()` (ptr.rs:584-636) already implements the suggested fix:
1. **try_inc_ref_from_zero** (ptr.rs:255-257) checks `dropping_state() != 0` internally — bug125 fix.
2. **Second check after try_inc_ref_from_zero** (ptr.rs:613-618): if `dropping_state() != 0 || has_dead_flag()`, the increment is undone via `dec_ref` and `None` is returned.

The TOCTOU window is closed by both the internal check in try_inc_ref_from_zero and the post-increment validation in try_upgrade.
