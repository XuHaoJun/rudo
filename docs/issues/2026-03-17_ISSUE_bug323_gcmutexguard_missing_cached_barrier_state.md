# [Bug]: GcMutexGuard 結構體未緩存 barrier 狀態，Drop 時重新讀取導致 TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需多執行緒：mutator 取得 guard 後，collector 啟動/停止 incremental marking |
| **Severity (嚴重程度)** | `Critical` | 導致年輕物件被錯誤回收，造成 use-after-free |
| **Reproducibility (復現難度)** | `Low` | 需精確時序，單執行緒無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcMutexGuard` in `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
在 `GcMutex::lock()` 取得 guard 時，應該緩存當時的 barrier 狀態，並將這些緩存值存儲在 guard struct 中。在 Drop 時，應該使用這些緩存值來決定是否觸發 barrier。

### 實際行為
bug316 聲稱已修復此問題，但實際上只修復了 `GcRwLockWriteGuard` 和 `GcThreadSafeRefMut`，`GcMutexGuard` 仍然存在此 bug：

**對比修復後的正確實現：**
1. **GcRwLockWriteGuard** (`sync.rs:433-438`):
   - Struct 正確存儲 `incremental_active` 和 `generational_active`
   - Drop 使用緩存值

2. **GcThreadSafeRefMut** (`cell.rs:1378-1383`):
   - Struct 正確存儲 `incremental_active` 和 `generational_active`
   - Drop 使用緩存值

**未修復的 GcMutexGuard：**
3. **GcMutexGuard** (`sync.rs:706-709`):
   - Struct **沒有**存儲 barrier 狀態欄位
   - Drop (lines 737-738) **重新調用** `is_incremental_marking_active()` 和 `is_generational_barrier_active()`

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題根源**: 
- `GcMutex::lock()` 正確緩存了 barrier 狀態 (sync.rs:594-595)
- 但緩存的值沒有存儲在 `GcMutexGuard` struct 中
- Drop 時重新檢查，創建 TOCTOU 窗口

**Race Condition 時序**:
1. Thread A: 調用 `GcMutex::lock()`，此時 `incremental_active = false`
2. Thread B: Collector 啟動 incremental marking，設置 `incremental_active = true`
3. Thread A: Guard Drop，重新檢查 `is_incremental_marking_active()` 返回 `true`
4. **問題**: 在 borrow 和 drop 之間，指針變化沒有被記錄到 SATB buffer

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒環境觸發 TOCTOU
// 此 bug 難以在單執行緒環境重現
// 建議使用 ThreadSanitizer 或設計特定時序的 stress test
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 在 `GcMutexGuard` struct 中添加 barrier 狀態欄位：
```rust
pub struct GcMutexGuard<'a, T: GcCapture + ?Sized> {
    guard: parking_lot::MutexGuard<'a, T>,
    _marker: PhantomData<&'a T>,
    incremental_active: bool,  // 新增
    generational_active: bool, // 新增
}
```

2. 在 `lock()` 和 `try_lock()` 中傳遞緩存值：
```rust
GcMutexGuard {
    guard,
    _marker: PhantomData,
    incremental_active,
    generational_active,
}
```

3. 在 Drop 中使用緩存值：
```rust
fn drop(&mut self) {
    // 使用 self.incremental_active 和 self.generational_active
    // 而不是重新調用 is_incremental_marking_active()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU race condition。bug316 聲稱已修復，但只修復了 RwLock 和 ThreadSafeCell，沒有修復 GcMutex。需要在 GcMutexGuard 中也實現相同的模式。

**Rustacean (Soundness 觀點):**
這不會導致明確的 UB，但會導致記憶體安全問題：物件被錯誤回收後可能被重用，導致 use-after-free。建議儘快修復。

**Geohot (Exploit 觀點):**
攻擊者可以透過觸發 GC 請求來控制 timing，精確地在第二次檢查後啟動 incremental marking，實現 memory corruption。

---

## Resolution (2026-03-21)

**Outcome:** Already fixed.

`GcMutexGuard` in `sync.rs` (lines 710–715) already has `incremental_active` and
`generational_active` fields. Both `lock()` (line 594–605) and `try_lock()` (line 635–647)
cache the barrier state after acquiring the lock and store it in the guard struct. The `Drop`
implementation (lines 741–763) reads `self.incremental_active` and `self.generational_active`
— the cached values — rather than re-querying global state. All sync tests pass (47/47).
