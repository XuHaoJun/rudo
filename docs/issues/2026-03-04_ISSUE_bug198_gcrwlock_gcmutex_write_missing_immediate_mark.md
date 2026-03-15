# [Bug]: GcRwLock::write / GcMutex::lock 缺少即時標記 - 與 GcCell 行為不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需多執行緒：mutator 執行 write()/lock() 與 collector 啟動標記的時序交錯 |
| **Severity (嚴重程度)** | `High` | 導致新指標指向的年輕物件被錯誤回收，造成 use-after-free |
| **Reproducibility (Reproducibility)** | `Low` | 需精確時序，單執行緒無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::write()`, `GcRwLock::try_write()`, `GcMutex::lock()`, `GcMutex::try_lock()`, `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcRwLock::write()` 和 `GcMutex::lock()` 應該與 `GcCell::borrow_mut()` 行為一致：在 mutation 完成後立即標記新的 GC 指針為黑色，確保在 generational barrier 或 incremental marking 啟用時，這些指標指向的物件不會被錯誤回收。

### 實際行為 (Actual Behavior)
在 `GcRwLock::write()` / `GcMutex::lock()` 中，新的 GC 指針**不會**被立即標記。標記被延遲到 `GcRwLockWriteGuard::drop()` / `GcMutexGuard::drop()` 時才執行。

這與 `GcCell::borrow_mut()` (lines 193-207) 的行為不同，後者會在 mutation 後立即調用 `mark_object_black()`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/sync.rs`:

```rust
// GcRwLock::write() (lines 250-264)
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();

    let guard = self.inner.write();
    record_satb_old_values_with_state(&*guard, incremental_active);
    self.trigger_write_barrier_with_state(generational_active, incremental_active);

    // 問題：沒有立即標記新的 GC 指針！
    // 標記被延遲到 GcRwLockWriteGuard::drop()

    GcRwLockWriteGuard {
        guard,
        _marker: PhantomData,
    }
}
```

對比 `GcCell::borrow_mut()` (lines 193-207):

```rust
if barrier_active {  // barrier_active = generational_active || incremental_active
    unsafe {
        let new_value = &*result;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            crate::heap::with_heap(|_heap| {
                for gc_ptr in new_gc_ptrs {
                    let _ = crate::gc::incremental::mark_object_black(
                        gc_ptr.as_ptr() as *const u8
                    );
                }
            });
        }
    }
}
```

**Race Condition 時序**:
1. Thread A (mutator): 執行 `write()`，mutation 完成，新 GC 指針存在於 value 中
2. Thread A: `write()` 返回，`GcRwLockWriteGuard` 被創建
3. Thread B (collector): 啟動 incremental/generational marking
4. Thread A: `GcRwLockWriteGuard` 被 drop（可能稍後才發生）
5. 在步驟 3 和 4 之間：新指標未被標記，可能導致其所指向的年輕物件被錯誤回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒環境觸發 race window
// 此 bug 難以在單執行緒環境重現
// 建議使用 ThreadSanitizer 或設計特定時序的 stress test
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcRwLock::write()` 和 `GcMutex::lock()` 中，在返回 write guard 之前，新增與 `GcCell::borrow_mut()` 相同的即時標記邏輯：

```rust
// 在 trigger_write_barrier_with_state() 之後，return GcRwLockWriteGuard 之前
let barrier_active = generational_active || incremental_active;

if barrier_active {
    unsafe {
        let guard_ref = &*guard;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        guard_ref.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            crate::heap::with_heap(|_heap| {
                for gc_ptr in new_gc_ptrs {
                    let _ = crate::gc::incremental::mark_object_black(
                        gc_ptr.as_ptr() as *const u8
                    );
                }
            });
        }
    }
}
```

同樣的修復應該應用於 `GcRwLock::try_write()` 和 `GcMutex::try_lock()`。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是典型的「延遲標記」與「即時標記」的權衡問題。即時標記確保物件在 mutation 後立即被保護，但會增加每次 mutation 的開銷。延遲標記（在 Drop 時）可以 batching，但會產生 race window。在generational GC 中，這個 window 特別危險，因為年輕物件可能在此期間被錯誤回收。

**Rustacean (Soundness 觀點):**
這不會導致明確的 UB（記憶體仍然有效），但會導致記憶體安全問題：物件被錯誤回收後可能被重用，導致 use-after-free。這種 bug 很難調試，因為它是並發的且依賴時序。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以透過精確控制 GC 時機來利用這個 race window。雖然難度較高，但這是一個確實存在的攻擊面。需要仔細考慮在何處添加同步點。

---

## 驗證記錄 (Verification Record)

**驗證日期:** 2026-03-04
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `crates/rudo-gc/src/sync.rs`:

1. **GcCell::borrow_mut()** (cell.rs lines 193-207): 
   - 獲取可變引用後，立即捕獲新的 GC 指針並調用 `mark_object_black()`
   - 這是正確的實現

2. **GcRwLock::write()** (sync.rs lines 250-264):
   - 獲取 write guard 後，記錄 SATB 舊值 (line 258)
   - 觸發 write barrier (line 259)
   - **但是沒有立即標記新的 GC 指針！**
   - 標記被延遲到 `GcRwLockWriteGuard::drop()` (lines 422-434)

3. **GcMutex::lock()** (sync.rs lines 533-547):
   - 與 GcRwLock::write() 相同的模式
   - 沒有立即標記新的 GC 指針

4. **Race Condition 確認:**
   - 在 `write()`/`lock()` 返回和 guard 被 drop 之間存在時間窗口
   - 在此窗口期間，如果 GC 啟動標記，新指標可能未被標記
   - 這與 `GcCell::borrow_mut()` 的行為不一致

**結論:** Bug 確認存在，需要修復以確保 `GcRwLock::write()`/`GcMutex::lock()` 與 `GcCell::borrow_mut()` 行為一致。

**注意:** 此 bug 與 bug192 (GcThreadSafeCell::borrow_mut 缺少即時標記) 為相同模式，但發生在不同的組件。

---

## Resolution (2026-03-14)

**Fixed.** Added `mark_gc_ptrs_immediate()` helper and invoked it in `GcRwLock::write()`, `GcRwLock::try_write()`, `GcMutex::lock()`, and `GcMutex::try_lock()` immediately after `trigger_write_barrier_with_state()`, before returning the guard. This aligns behavior with `GcCell::borrow_mut()` so new GC pointers are marked on acquisition rather than deferred to guard drop. All sync tests pass.
