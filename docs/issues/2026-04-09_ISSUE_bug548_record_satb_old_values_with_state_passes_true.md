# [Bug]: record_satb_old_values_with_state called with `true` instead of actual `incremental_active` value

**Status:** Fixed
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | When incremental marking is NOT active |
| **Severity (嚴重程度)** | Medium | Unnecessary work, potential consistency issues |
| **Reproducibility (復現難度)** | Low | Hard to observe impact directly |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sync.rs` - GcRwLock::write, GcRwLock::try_write, GcMutex::lock, GcMutex::try_lock
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`record_satb_old_values_with_state` should only record SATB old values when incremental marking is active, as controlled by its `incremental_active` parameter:

```rust
fn record_satb_old_values_with_state<T: GcCapture + ?Sized>(value: &T, incremental_active: bool) {
    if !incremental_active {
        return;  // Early exit when NOT active
    }
    // ... record old values ...
}
```

### 實際行為 (Actual Behavior)

All callers pass `true` instead of the actual `incremental_active` value:

- `GcRwLock::write()` (line 290): `record_satb_old_values_with_state(&*guard, true)`
- `GcRwLock::try_write()` (line 331): `record_satb_old_values_with_state(&*guard, true)`
- `GcMutex::lock()` (line 596): `record_satb_old_values_with_state(&*guard, true)`
- `GcMutex::try_lock()` (line 635): `record_satb_old_values_with_state(&*guard, true)`

This means SATB old values are always recorded even when incremental marking is inactive.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `sync.rs`, the barrier state is correctly cached:
```rust
let incremental_active = is_incremental_marking_active();
let generational_active = is_generational_barrier_active();
record_satb_old_values_with_state(&*guard, true);  // BUG: should be incremental_active
self.trigger_write_barrier_with_state(generational_active, incremental_active);
mark_gc_ptrs_immediate(&*guard, generational_active || incremental_active);
```

The `incremental_active` variable is correctly obtained but then `true` is passed to `record_satb_old_values_with_state` instead of `incremental_active`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// When incremental marking is NOT active, the function should early-return
// But it always does work because `true` is passed instead of `incremental_active`
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change all four call sites to use `incremental_active` instead of `true`:

```rust
record_satb_old_values_with_state(&*guard, incremental_active);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB barrier should only fire during incremental marking to avoid performance overhead.

**Rustacean (Soundness 觀點):**
Passing wrong boolean is a logic bug - function is designed to guard on the parameter but callers ignore the actual state.

**Geohot (Exploit 觀點):**
No security impact but incorrect barrier behavior could cause subtle GC issues.