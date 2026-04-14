# [Bug]: GcScope::spawn comments reference bugXXX instead of bug496

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Very High` | All code references bugXXX without proper bug number |
| **Severity (嚴重程度)** | `Low` | No functional impact - code is correct |
| **Reproducibility (復現難度)** | `N/A` | Not a functional bug |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcScope::spawn` (handles/async.rs:1322-1328)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

在 `handles/async.rs` 中，`GcScope::spawn` 的generation 檢查相關註釋仍然使用 `bugXXX` 佔位符，而非正式的 bug 編號 `bug496`。

### 預期行為 (Expected Behavior)
代碼註釋應該引用正式的 bug 編號 `bug496`，以便追蹤和文檔一致性。

### 實際行為 (Actual Behavior)
代碼註釋仍然使用 `bugXXX` 佔位符：
```rust
// Line 1322: // Get generation BEFORE dereference to detect slot reuse (bugXXX).
// Line 1328: // FIX bugXXX: Verify generation hasn't changed (slot was NOT reused).
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在提交 `90a4031ae156ee63bf1a987eeba295be6f66e85e` 中添加了 generation check 修復後，代碼注釋未更新為正式的 bug 編號。

後續在 `8302e2d1d0d6ecbe26a79b4843674cfc9d5cc009` 中創建了 bug496 issue，但 comment 中的 `bugXXX` 佔位符未被替換。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```bash
# 搜索 bugXXX 引用
grep -rn "bugXXX" crates/rudo-gc/src/
```

預期：找到 0 個結果
實際：找到 2 個結果（async.rs:1322, async.rs:1328）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `handles/async.rs` 中的 `bugXXX` 替換為 `bug496`：
1. Line 1322: `bugXXX` → `bug496`
2. Line 1328: `bugXXX` → `bug496`

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
代碼功能正確，generation check 有效防止 slot reuse TOCTOU。這只是文檔/追蹤問題。

**Rustacean (Soundness 觀點):**
無 soundness 問題，純粹是註釋一致性問題。

**Geohot (Exploit 觀點):**
無 exploit 風險。

---

## 備註

此 bug 為文檔追蹤問題，發現於 `start-bug-hunt` 工作流。代碼實際功能正確，generation check 已正確實作。