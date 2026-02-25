# [Bug]: sync.rs trigger_write_barrier TOCTOU - is_incremental_marking_active called twice

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要並發 incremental marking phase 改變 |
| **Severity (嚴重程度)** | `Medium` | 可能導致 write barrier 失效或不必要的 barrier |
| **Reproducibility (復現難度)** | `High` | 需精確時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::trigger_write_barrier`, `GcMutex::trigger_write_barrier`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+ (incremental marking)

---

## 📝 問題描述 (Description)

在 `sync.rs:108-109` 的 `GcRwLock::trigger_write_barrier` 函數中，以及 `sync.rs:427-428` 的 `GcMutex::trigger_write_barrier` 函數中，`is_incremental_marking_active()` 被調用兩次。這造成 TOCTOU (Time-of-check to time-of-use) 競爭條件。

**注意**：此問題與 bug100 類似，但 bug100 只記錄了 `cell.rs` 中的 `GcCell::trigger_write_barrier`，而此 bug 記錄 `sync.rs` 中 `GcRwLock` 和 `GcMutex` 的相同問題。

### 預期行為
`trigger_write_barrier` 應該在檢查和調用之間保持一致的狀態。

### 實際行為
`is_incremental_marking_active()` 在 `if` 條件中調用一次，然後在調用 `unified_write_barrier` 時又調用一次。如果 phase 在兩次調用之間改變，會導致：
- 進入不應進入的分支
- 傳遞給 barrier 函數的狀態與檢查時不一致

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點 1：** `sync.rs:105-111` (`GcRwLock::trigger_write_barrier`)
```rust
fn trigger_write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    if is_generational_barrier_active() || is_incremental_marking_active() {  // <-- 第一次調用
        crate::heap::unified_write_barrier(ptr, is_incremental_marking_active()); // <-- 第二次調用
    }
}
```

**問題點 2：** `sync.rs:424-430` (`GcMutex::trigger_write_barrier`)
```rust
fn trigger_write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    if is_generational_barrier_active() || is_incremental_marking_active() {  // <-- 第一次調用
        crate::heap::unified_write_barrier(ptr, is_incremental_marking_active()); // <-- 第二次調用
    }
}
```

問題：
1. `is_incremental_marking_active()` 讀取 `IncrementalMarkState::phase()`
2. 使用 `Ordering::Relaxed` 讀取 phase
3. 兩次調用之間，phase 可能從 `Marking` 變為其他值，反之亦然

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確控制時序：
1. 啟動 incremental marking
2. 在 `trigger_write_barrier` 的兩次調用之間中斷
3. 改變 phase

理論上可能導致：
- Write barrier 不應觸發時觸發（性能損失）
- Write barrier 應觸發時未觸發（記憶體錯誤 - 年輕物件被錯誤回收）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**GcRwLock::trigger_write_barrier:**
```rust
fn trigger_write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    
    if generational_active || incremental_active {
        crate::heap::unified_write_barrier(ptr, incremental_active);
    }
}
```

**GcMutex::trigger_write_barrier:**
```rust
fn trigger_write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    
    if generational_active || incremental_active {
        crate::heap::unified_write_barrier(ptr, incremental_active);
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 可能導致 SATB 屏障失效。如果在檢查時 phase=Marking，但傳遞給 barrier 時 phase 變為其他值，則可能跳過必要的屏障記錄，導致年輕代物件被錯誤回收。

**Rustacean (Soundness 觀點):**
這是並發安全問題。使用 Relaxed ordering 讀取 phase，且在兩次讀取之間無同步，导致可觀察的競爭行為。

**Geohot (Exploit 觀點):**
在高負載並發環境中，攻擊者可能利用此 TOCTOU 觸發不一致的 barrier 行為，進一步利用記憶體管理漏洞。

---

**相關 Bug:**
- bug100: cell.rs GcCell::trigger_write_barrier TOCTOU (相同模式，不同位置)

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

Cached `is_incremental_marking_active()` and `is_generational_barrier_active()` in local variables before the if block in both `GcRwLock::trigger_write_barrier` and `GcMutex::trigger_write_barrier` (sync.rs). Eliminates TOCTOU between check and use.
