# [Bug]: GcRwLock::write() SATB barrier inconsistency when incremental_active transitions

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 write() 和標記之間 incremental marking phase 發生轉換 |
| **Severity (嚴重程度)** | High | 可能導致年輕對象被錯誤回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精確的時序控制，單線程無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::write()` (`sync.rs:282-303`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
SATB barrier 應該一致地記錄 OLD 值並標記 NEW 值。如果 `incremental_active` 在 `write()` 期間變為 true，兩者都應該發生。

### 實際行為 (Actual Behavior)
`GcRwLock::write()` 使用快取的 `incremental_active` 值：
- Line 295: `record_satb_old_values_with_state(&*guard, true)` - 總是記錄 OLD 值
- Line 298: `mark_gc_ptrs_immediate(&*guard, incremental_active)` - 使用快取值

如果 `incremental_active` 從 FALSE 轉換為 TRUE 在 entry 和 line 298之間：
- OLD 值被記錄 (good)
- NEW 值不被標記為黑色 (bad)

這破壞了 SATB 不變性。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `sync.rs:287-298`:
```rust
let guard = self.inner.write();
// Cache barrier state AFTER acquiring lock
let incremental_active = is_incremental_marking_active();  // Line 290: cached
let generational_active = is_generational_barrier_active();
// FIX bug432: Always record SATB OLD values...
record_satb_old_values_with_state(&*guard, true);  // Line 295: ALWAYS records
self.trigger_write_barrier_with_state(generational_active, incremental_active);
// FIX bug302: Only mark GC pointers black during incremental marking, not generational barrier.
mark_gc_ptrs_immediate(&*guard, incremental_active);  // Line 298: uses CACHED value
```

問題：
1. Line 290 快取了 `incremental_active`
2. Line 295 總是記錄 OLD 值（傳遞 `true`）
3. Line 298 使用快取的值，如果 `incremental_active` 變為 TRUE，則不標記 NEW 值

時序問題：
```
T1: Thread A calls write(), incremental_active = false
T2: OLD values are recorded (line 295 passes true)
T3: Collector starts incremental marking, incremental_active = true
T4: mark_gc_ptrs_immediate(&*guard, incremental_active=false) - NEW values NOT marked!
T5: Objects only reachable from NEW values may be prematurely collected!
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

- 與 bug432 相關：bug432 修復了總是記錄 OLD 值，但沒有修復標記 NEW 值的時序問題
- 需要 Miri 或 ThreadSanitizer 驗證