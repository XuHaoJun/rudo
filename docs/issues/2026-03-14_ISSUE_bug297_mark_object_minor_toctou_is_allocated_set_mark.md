# [Bug]: mark_object_minor TOCTOU between is_allocated and set_mark

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep and minor marking |
| **Severity (嚴重程度)** | `High` | Can corrupt GC's live object tracking |
| **Reproducibility (復現難度)** | `High` | Needs concurrent execution with precise timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_minor` in `gc/gc.rs`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The function should atomically check if a slot is allocated AND set the mark bit, preventing race conditions with lazy sweep.

### 實際行為 (Actual Behavior)
There is a TOCTOU race between the `is_allocated` check (line 2071) and `set_mark` call (line 2079). A slot can be:
1. Swept by lazy sweep between the check and mark
2. Reallocated with a new object  
3. The mark incorrectly set on the new object

---

## 🔬 根本原因分析 (Root Cause Analysis)

The code checks `is_allocated`, then checks `is_marked`, then calls `set_mark` - all as separate operations. Between the initial `is_allocated` check and the `set_mark` call, lazy sweep can reclaim the slot and reallocate it to a new object.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// PoC would require:
// 1. Thread A: Call mark_object_minor on slot X
// 2. Thread B: Lazy sweep reclaims slot X, allocates new object
// 3. Thread A: set_mark incorrectly marks new object
// This is timing-dependent and difficult to reproduce reliably
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Use the `try_mark` pattern from `scan_page_for_unmarked_refs` (incremental.rs:810-845) which atomically checks allocation status and marks in one operation.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Classic mark-sweep concurrency bug. The interaction between incremental/parallel marking and lazy sweep creates a race window. The correct fix is to use atomic try_mark + recheck pattern.

**Rustacean (Soundness 觀點):**
Undefined behavior - slot can be reused after is_allocated check, causing mark on wrong object. This corrupts GC's heap view.

**Geohot (Exploit 觀點):**
Exploitable with precise timing. Can prevent collection of malicious objects or cause heap corruption.
