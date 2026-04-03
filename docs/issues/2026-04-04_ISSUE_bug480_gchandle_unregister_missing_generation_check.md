# [Bug]: GcHandle::unregister() missing generation check (bug407 pattern)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 slot sweep + reuse 在 unregister 窗口期間發生 |
| **Severity (嚴重程度)** | Critical | 導致 ref_count 損壞，可能造成 use-after-free 或 double-free |
| **Reproducibility (復現難度)** | Medium | 需要精確時序，但 stress 測試可復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::unregister()` in `cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`GcHandle::unregister()` 方法缺少 generation 檢查，而 `GcHandle::drop()` 已在 bug407 修復中添加了相同檢查。如果 slot 在移除 handle 和呼叫 `dec_ref()` 之間被 sweep 並重新分配，會導致 `dec_ref()` 被呼叫在新物件上，損壞新物件的 ref_count。

### 預期行為 (Expected Behavior)
`unregister()` 應在 `dec_ref()` 前檢查 generation，確保 slot 未被重用。

### 實際行為 (Actual Behavior)
`unregister()` 直接呼叫 `dec_ref()` 而不檢查 generation，可能導致 ref_count 損壞。

---

## 🔬 根本原因分析 (Root Cause Analysis)

`drop()` 實作（lines 812-847）有完整的 generation 檢查：
1. 在移除前取得 `pre_generation`
2. 移除 handle from root set
3. 設定 `handle_id = INVALID`
4. 檢查 `is_allocated`
5. 驗證 `current_generation == pre_generation`
6. 確認後才呼叫 `dec_ref()`

但 `unregister()` 實作（lines 117-136）只有：
1. 移除 handle from root set
2. 設定 `handle_id = INVALID`
3. 直接呼叫 `dec_ref()` - 無 generation 檢查！

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並髏�ephemeron GC stress test + concurrent unregister。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `unregister()` 中新增與 `drop()` 相同的 generation 檢查：
1. 在移除前取得 `pre_generation`
2. 在 `dec_ref()` 前驗證 generation 未變

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bug407 修復了 `drop()` 中的 TOCTOU，但 `unregister()` 使用相同模式卻未修復。Slot reuse 檢測對於 GC 正確性至關重要。

**Rustacean (Soundness 觀點):**
這是經典的 TOCTOU (Time-Of-Check-Time-Of-Use) bug。Generation check 是防止此類問題的標準模式。

**Geohot (Exploit 觀點):**
Ref_count 損壞可導致 UAF 或 double-free。在 concurrent 場景下可進一步利用。
