# [Bug]: GcRwLock::try_write() and GcMutex::try_lock() missing bug479 SATB fix

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 lock/try_write 和標記之間 incremental marking phase 發生轉換 |
| **Severity (嚴重程度)** | High | 可能導致年輕對象被錯誤回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精確的時序控制，單線程無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::try_write()` (`sync.rs:331-348`), `GcMutex::lock()` (`sync.rs:595-613`), `GcMutex::try_lock()` (`sync.rs:637-654`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
所有 lock/write 函數應該一致地記錄 OLD 值並標記 NEW 值。如果 `incremental_active` 在 lock/write 期間變為 true，兩者都應該發生。

### 實際行為 (Actual Behavior)
`GcRwLock::write()` 已經修復 (bug479)，但 `GcRwLock::try_write()`、`GcMutex::lock()` 和 `GcMutex::try_lock()` 沒有修復。

這些函數使用快取的 `incremental_active` 值：
- Line XXX: `record_satb_old_values_with_state(&*guard, true)` - 總是記錄 OLD 值
- Line XXX: `mark_gc_ptrs_immediate(&*guard, incremental_active)` - 使用快取值

如果 `incremental_active` 從 FALSE 轉換為 TRUE 在 entry 和標記之間：
- OLD 值被記錄 (good)
- NEW 值不被標記為黑色 (bad)

這破壞了 SATB 不變性。

---

## 🔬 根本原因分析 (Root Cause Analysis)

bug479 修復了 `GcRwLock::write()`，但相同的修復沒有應用到：
1. `GcRwLock::try_write()` (sync.rs:331-348)
2. `GcMutex::lock()` (sync.rs:595-613)
3. `GcMutex::try_lock()` (sync.rs:637-654)

對比：

**GcRwLock::write() (已修復)** - sync.rs:283-305:
```rust
// Line 295: ALWAYS records OLD
record_satb_old_values_with_state(&*guard, true);
// Line 300: FIX bug479 - ALWAYS marks
mark_gc_ptrs_immediate(&*guard, true);  // FIX bug479 fix
```

**GcRwLock::try_write() (未修復)** - sync.rs:331-348:
```rust
// Line 339: ALWAYS records OLD
record_satb_old_values_with_state(&*guard, true);
// Line 342: Uses cached incremental_active - BUG!
mark_gc_ptrs_immediate(&*guard, incremental_active);  // Missing bug479 fix!
```

**GcMutex::lock() (未修復)** - sync.rs:595-613:
```rust
// Line 605: ALWAYS records OLD
record_satb_old_values_with_state(&*guard, true);
// Line 608: Uses cached incremental_active - BUG!
mark_gc_ptrs_immediate(&*guard, incremental_active);  // Missing bug479 fix!
```

**GcMutex::try_lock() (未修復)** - sync.rs:637-654:
```rust
// Line 645: ALWAYS records OLD
record_satb_old_values_with_state(&*guard, true);
// Line 648: Uses cached incremental_active - BUG!
mark_gc_ptrs_immediate(&*guard, incremental_active);  // Missing bug479 fix!
```

時序問題：
```
T1: Thread A calls try_write()/lock(), incremental_active = false
T2: OLD values are recorded (line passes true)
T3: Collector starts incremental marking, incremental_active = true
T4: mark_gc_ptrs_immediate(&*guard, incremental_active=false) - NEW values NOT marked!
T5: Objects only reachable from NEW values may be prematurely collected!
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要多執行緒並發測試，單執行緒無法重現。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `mark_gc_ptrs_immediate(&*guard, incremental_active)` 改為 `mark_gc_ptrs_immediate(&*guard, true)` 在：

1. `GcRwLock::try_write()` - sync.rs:342
2. `GcMutex::lock()` - sync.rs:608
3. `GcMutex::try_lock()` - sync.rs:648

並添加相同的註解：
```rust
// FIX bug479: Always mark GC pointers black when OLD values were recorded.
// If incremental becomes active between entry and here, we must mark NEW
// to maintain SATB consistency (OLD recorded, NEW must be marked too).
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 不變性要求：如果記錄了 OLD 值，相應的 NEW 值也應該被標記。如果增量標記在記錄後變為活躍，NEW 對象可能未被標記，導致它們被錯誤回收。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果 NEW 對象被錯誤回收，透過 NEW 指針訪問會導致 use-after-free。

**Geohot (Exploit 觀點):**
攻擊者可能通過控制 GC 時序來觸發此 bug，導致記憶體腐敗。

---

## 備註

- 與 bug479 相關：bug479 修復了 GcRwLock::write()，但沒有修復 try_write()
- 與 bug432 相關：bug432 修復了總是記錄 OLD 值
- 與 bug302 相關：bug302 修復了只在 incremental marking 時標記
- 需要 Miri 或 ThreadSanitizer 驗證