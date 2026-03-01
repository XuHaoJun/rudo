# [Bug]: GcRwLock/GcMutex write/lock TOCTOU - record_satb_old_values 與 trigger_write_barrier 狀態不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking 狀態在兩次檢查之間改變，發生機率中等 |
| **Severity (嚴重程度)** | High | TOCTOU 可能導致 SATB 不變性破壞，造成年輕物件被錯誤回收 |
| **Reproducibility (復現難度)** | High | 需要精確時序控制來觸發狀態改變 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::write()`, `GcRwLock::try_write()`, `GcMutex::lock()`, `GcMutex::try_lock()` (sync.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcRwLock::write()` 和 `GcMutex::lock()` 應該與 `GcCell::borrow_mut()` 一致，在整個函數中緩存 barrier 狀態，確保 `record_satb_old_values` 和 `trigger_write_barrier` 使用一致的狀態。

### 實際行為 (Actual Behavior)
`GcRwLock::write()` (sync.rs:247-259) 和 `GcMutex::lock()` (sync.rs:525-536) 分別調用：
1. `record_satb_old_values(&*guard)` - 內部檢查 `is_incremental_marking_active()`
2. `self.trigger_write_barrier()` - 再次檢查 `is_incremental_marking_active()` 和 `is_generational_barrier_active()`

如果 barrier 狀態在這兩次檢查之間改變，會導致：
- SATB 記錄了舊值但沒有觸發 barrier（狀態從 ACTIVE 變為 INACTIVE）
- 沒有記錄 SATB 但觸發了 barrier（狀態從 INACTIVE 變為 ACTIVE）

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `sync.rs` 中：

```rust
// GcRwLock::write() - sync.rs:247-259
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.write();
    record_satb_old_values(&*guard);  // 內部調用 is_incremental_marking_active()
    self.trigger_write_barrier();      // 再次檢查 is_incremental_marking_active() 和 is_generational_barrier_active()
    GcRwLockWriteGuard {
        guard,
        _marker: PhantomData,
    }
}
```

`record_satb_old_values` 函數 (sync.rs:57-81) 內部調用 `is_incremental_marking_active()`:
```rust
fn record_satb_old_values<T: GcCapture + ?Sized>(value: &T) {
    if !is_incremental_marking_active() {  // 第一次檢查
        return;
    }
    // ...
}
```

而 `trigger_write_barrier` (sync.rs:136-144) 又檢查一次：
```rust
fn trigger_write_barrier(&self) {
    let incremental_active = is_incremental_marking_active();  // 第二次檢查
    let generational_active = is_generational_barrier_active();
    if generational_active || incremental_active {
        crate::heap::unified_write_barrier(ptr, incremental_active);
    }
}
```

對比 `GcCell::borrow_mut()` (cell.rs:155-208) 的正確實現：
```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    // 緩存狀態一次
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    
    if incremental_active {
        // 使用緩存的值
        record_satb_old_values(...);
    }
    
    crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut", incremental_active);  // 使用緩存的值
    
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒環境觸發 TOCTOU
// 1. Thread A: 調用 GcRwLock::write() 或 GcMutex::lock()
// 2. Thread B: 在 Thread A 調用 record_satb_old_values 和 trigger_write_barrier 之間改變 barrier 狀態
// 3. 觀察到不一致的 barrier 行為
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 在 `write()` / `try_write()` / `lock()` / `try_lock()` 中緩存 barrier 狀態：
```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    // 緩存 barrier 狀態
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    
    let guard = self.inner.write();
    
    // 使用緩存的狀態
    if incremental_active {
        record_satb_old_values_with_state(&*guard, incremental_active);
    }
    
    if generational_active || incremental_active {
        self.trigger_write_barrier_with_state(generational_active, incremental_active);
    }
    
    GcRwLockWriteGuard {
        guard,
        _marker: PhantomData,
    }
}
```

2. 或修改 `record_satb_old_values` 接受 barrier 狀態參數，避免重複檢查。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB barrier 的正確性依賴於「記錄 old 指針」和「標記操作」的原子性。如果兩者使用不同的 barrier 狀態，會破壞 SATB 不變性，導致增量標記期間遺漏年輕物件的引用。

**Rustacean (Soundness 觀點):**
這是一個經典的 TOCTOU (Time-of-check to time-use) 漏洞。與 bug110、bug116 是同一模式，但發生在不同的 API 點。GcCell::borrow_mut 已經正確緩存了狀態，但 GcRwLock 和 GcMutex 沒有遵循相同的模式。

**Geohot (Exploit 觀點):**
雖然利用這個 TOCTOU 需要精確時序控制，但在高負載的 GC 環境中，incremental marking 的狀態可能會頻繁切換。攻擊者可能通過控制 GC 調度來觸發這個窗口。

---

## 相關 Bug

- bug101: sync.rs GcRwLock::trigger_write_barrier / GcMutex::trigger_write_barrier TOCTOU (相同模式)
- bug110: GcCell::borrow_mut 三次調用 is_incremental_marking_active 導致 TOCTOU (已修復)
- bug116: GcThreadSafeCell::borrow_mut() TOCTOU (已修復)
- bug161: GcRwLock/GcMutex Drop TOCTOU (不同位置)

---

## Resolution (2026-03-02)

**Outcome:** Already fixed.

The fix was applied prior to this verification. The current implementation in `sync.rs` correctly caches barrier state at the start of `write()`, `try_write()`, `lock()`, and `try_lock()`:

```rust
let incremental_active = is_incremental_marking_active();
let generational_active = is_generational_barrier_active();
let guard = self.inner.write();
record_satb_old_values_with_state(&*guard, incremental_active);  // uses cached state
self.trigger_write_barrier_with_state(generational_active, incremental_active);  // uses cached state
```

Both `record_satb_old_values_with_state` and `trigger_write_barrier_with_state` accept the cached state as parameters, eliminating the TOCTOU window. Behavior now matches `GcCell::borrow_mut()` as described in the issue.
