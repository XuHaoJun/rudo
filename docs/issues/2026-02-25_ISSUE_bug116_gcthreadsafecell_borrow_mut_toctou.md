# [Bug]: GcThreadSafeCell::borrow_mut() TOCTOU - 兩處 is_incremental_marking_active() 調用導致狀態不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 line 943 和 line 972 之間 incremental marking 狀態改變 |
| **Severity (嚴重程度)** | Medium | 導致 barrier 行為不一致，可能遺漏或錯誤執行 barrier |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut()` (cell.rs:937-978)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

在 `GcThreadSafeCell::borrow_mut()` 中，`is_incremental_marking_active()` 在兩個不同位置被調用：

1. **Line 943**: 第一次調用，檢查是否需要捕獲 SATB old values
2. **Line 972**: 調用 `trigger_write_barrier()`，其內部再次調用 `is_incremental_marking_active()`

### 預期行為
應該在整個 `borrow_mut()` 函數中使用一致的 incremental marking 狀態，確保 SATB 捕獲和 write barrier 執行的行為一致。

### 實際行為
在 line 943 和 line 972 之間，incremental marking 階段可能改變（例如 GC 完成或 fallback 觸發）。這會導致：
- Line 943: 如果 incremental marking 為 ACTIVE，則捕獲 SATB old values
- Line 972: 如果 incremental marking 變為 INACTIVE，則 `trigger_write_barrier()` 會執行不同的邏輯

這造成 barrier 行為不一致 - 有些操作使用「active」路徑，有些使用「inactive」路徑。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/cell.rs:937-978`:

```rust
pub fn borrow_mut(&self) -> GcThreadSafeRefMut<'_, T>
where
    T: Trace + GcCapture,
{
    let guard = self.inner.lock();

    // Line 943: 第一次調用
    if crate::gc::incremental::is_incremental_marking_active() {
        // 捕獲 SATB old values...
    }

    // Line 972: trigger_write_barrier() 內部再次調用
    self.trigger_write_barrier();

    GcThreadSafeRefMut { ... }
}
```

問題：
1. Line 943 調用 `is_incremental_marking_active()` 決定是否捕獲 SATB
2. Line 972 調用 `trigger_write_barrier()`，其內部再次調用 `is_incremental_marking_active()` (參考 bug111)
3. 這兩次調用之間的狀態可能改變

這與以下現有 bug 相關但不同：
- **bug110**: GcCell::borrow_mut() - 涉及三次調用
- **bug111**: GcThreadSafeCell::trigger_write_barrier() - 涉及內部雙重調用
- **bug116**: GcThreadSafeCell::borrow_mut() - 涉及在兩個函數中各調用一次

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確控制時序：
1. 調用 `borrow_mut()` 
2. 在 line 943 執行後、line 972 執行前，incremental marking 狀態改變
3. 導致 SATB 捕獲和 barrier 執行使用不一致的狀態

理論上需要並發 GC 和 mutator 才能穩定重現。建議使用 model checker 驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

緩存 `is_incremental_marking_active()` 的結果並在整個函數中使用：

```rust
pub fn borrow_mut(&self) -> GcThreadSafeRefMut<'_, T>
where
    T: Trace + GcCapture,
{
    let guard = self.inner.lock();

    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    
    if incremental_active {
        // 捕獲 SATB old values...
    }

    // 使用 cached 值而不是再次調用
    if incremental_active {
        self.trigger_write_barrier();
    }

    GcThreadSafeRefMut { ... }
}
```

或者，在 `trigger_write_barrier()` 內部也使用同樣的緩存策略（這將同時修復 bug111）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 可能導致 write barrier 行為不一致。當 incremental marking 狀態在兩次檢查之間改變時，SATB 記錄可能與實際 barrier 執行不匹配，導致標記階段的記憶體不一致。

**Rustacean (Soundness 觀點):**
這是並發安全問題。連續調用同一函數卻可能得到不同結果，違反了函數的穩定性和可預測性原則。

**Geohot (Exploit 觀點):**
在高負載並發環境中，攻擊者可能嘗試在兩次檢查之間觸發 GC 狀態改變，利用不一致的 barrier 行為實現記憶體破壞。

---

## Resolution (2026-02-27)

**Outcome:** Fixed.

Cached `is_incremental_marking_active()` once at the start of `borrow_mut()` and use it for both:
1. The SATB capture block (lines 1044–1071)
2. The write barrier via new `trigger_write_barrier_with_incremental(incremental_active)`

`trigger_write_barrier()` now delegates to `trigger_write_barrier_with_incremental()` with the cached value when called from `borrow_mut()`. Other callers (`borrow_mut_simple`) still use the original `trigger_write_barrier()` which computes the value internally.
