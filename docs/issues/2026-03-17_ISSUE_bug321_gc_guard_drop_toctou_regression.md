# [Bug]: Write Guard Drop TOCTOU - Barrier States Still Re-checked (Regression of bug316)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需多執行緒：mutator 取得 guard 後，collector 啟動 incremental marking |
| **Severity (嚴重程度)** | `Critical` | 導致年輕物件被錯誤回收，造成 use-after-free |
| **Reproducibility (復現難度)** | `Low` | 需精確時序，單執行緒無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeRefMut::drop` (cell.rs:1391-1416), `GcRwLockWriteGuard::drop` (sync.rs:458-483)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
在 `borrow_mut()` / `write()` 取得 guard 時，應該緩存當時的 barrier 狀態 (`incremental_active`, `generational_active`)，並將這些緩存值存儲在 guard struct 中。在 Drop 時，應該使用這些緩存值來決定是否觸發 barrier，而不是重新檢查 barrier 狀態。

### 實際行為
bug316 已經報告了這個問題，但當前程式碼仍然在 Drop 時重新檢查 barrier 狀態。這可能是：
1. bug316 的修復從未被實際應用
2. 或後來發生了 regression

**GcThreadSafeRefMut** (`cell.rs:1393-1394`):
```rust
fn drop(&mut self) {
    // BUG: 重新檢查 barrier 狀態，而非使用 borrow_mut() 時緩存的值
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    // ...
}
```

**GcRwLockWriteGuard** (`sync.rs:460-461`):
```rust
fn drop(&mut self) {
    // BUG: 重新檢查 barrier 狀態，而非使用 write() 時緩存的值
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    // ...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題根源**: 
- `borrow_mut()` (cell.rs:1057-1058) 和 `write()` (sync.rs:284-285) 正確緩存了 barrier 狀態
- 但緩存的值沒有存儲在 guard struct 中 (`GcThreadSafeRefMut` 在 cell.rs:1109-1112, `GcRwLockWriteGuard` 在 sync.rs:290-293)
- Drop 時重新檢查，創建 TOCTOU 窗口

**Race Condition 時序**:
1. Thread A: 調用 `borrow_mut()`，此時 `incremental_active = false`
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

在 guard struct 中存儲緩存的 barrier 狀態：

```rust
pub struct GcThreadSafeRefMut<'a, T: GcCapture + ?Sized> {
    inner: parking_lot::MutexGuard<'a, T>,
    _marker: std::marker::PhantomData<&'a mut T>,
    incremental_active: bool,  // 新增
    generational_active: bool, // 新增
}
```

在 `borrow_mut()` 中設置：
```rust
GcThreadSafeRefMut {
    inner: guard,
    _marker: std::marker::PhantomData,
    incremental_active, // 從緩存的值傳入
    generational_active,
}
```

在 Drop 中使用緩存值：
```rust
fn drop(&mut self) {
    // 使用 self.incremental_active 和 self.generational_active
    // 而不是重新調用 is_incremental_marking_active()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU race condition。bug316 已經識別了這個問題，但修復似乎從未被應用到程式碼庫中。這個 regression 表明需要更嚴格的回歸測試來確保 bug 修復不會丟失。

**Rustacean (Soundness 觀點):**
這不會導致明確的 UB，但會導致記憶體安全問題：物件被錯誤回收後可能被重用，導致 use-after-free。建議儘快修復。

**Geohot (Exploit 觀點):**
攻擊者可以透過觸發 GC 請求來控制 timing，精確地在第二次檢查後啟動 incremental marking，實現 memory corruption。
