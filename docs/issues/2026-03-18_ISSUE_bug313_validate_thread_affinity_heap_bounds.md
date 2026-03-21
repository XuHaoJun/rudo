# [Bug]: validate_thread_affinity 使用 `>` 進行 heap bounds 檢查，與 `is_in_range` 不一致

**Status:** Invalid
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需觸發記憶體邊界條件 |
| **Severity (嚴重程度)** | High | 可能導致 UB |
| **Reproducibility (復現難度)** | High | 需指標恰好等於 heap_end |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `validate_thread_affinity` in `cell.rs:267` and `handles/async.rs:118`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
使用 `is_in_range` 的語意進行 heap bounds 檢查：`(ptr_addr < heap_start) || (ptr_addr >= heap_end)`

### 實際行為 (Actual Behavior)
使用 `>` 而非 `>=`：`if ptr_addr < heap_start || ptr_addr > heap_end`

---

## 🔬 根本原因分析 (Root Cause Analysis)
`is_in_range` 函數（heap.rs:1829）使用專屬上界語意：
```rust
addr >= self.min_addr && addr < self.max_addr
```

但在 `cell.rs:267` 和 `handles/async.rs:118` 中，bounds check 錯誤地使用 `>`：
```rust
if ptr_addr < heap_start || ptr_addr > heap_end
```

當 `ptr_addr == heap_end` 時，`ptr_addr > heap_end` 為 false，導致函數不會提前返回，進而呼叫 `ptr_to_page_header` 在無效指標上，可能造成 UB。

此問題與 bug253 類似，但 bug253 修復的是 heap.rs 中的 write barrier 函數，而這兩處未被修復。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)
需要構造指標恰好等於 heap_end 的情況，難以穩定重現。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)
將 `cell.rs:267` 和 `handles/async.rs:118` 的 `>` 改為 `>=`：
```rust
if ptr_addr < heap_start || ptr_addr >= heap_end
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bounds check 語意必須與 heap 範圍定義一致，否則會導致指標被錯誤處理。

**Rustacean (Soundness 觀點):**
在無效指標上呼叫 `ptr_to_page_header` 構成 UB。

**Geohot (Exploit 觀點):**
指標恰好等於 heap_end 是罕見邊界條件，但仍屬記憶體安全漏洞。

---

## Resolution (2026-03-21)

**Outcome:** Invalid — already fixed in current code.

Both locations use `>=` (exclusive upper bound) matching `is_in_range` semantics:
- `cell.rs:273`: `if ptr_addr < heap_start || ptr_addr >= heap_end`
- `handles/async.rs:118`: `if ptr_addr < heap_start || ptr_addr >= heap_end`

The fix was applied before this investigation. No code change needed.
