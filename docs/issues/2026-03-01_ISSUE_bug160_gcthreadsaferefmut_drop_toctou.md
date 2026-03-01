# [Bug]: GcThreadSafeRefMut::drop TOCTOU 導致 barrier 遺漏

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需多執行緒：mutator drop 與 collector 啟動 incremental marking 的時序交錯 |
| **Severity (嚴重程度)** | `High` | 導致年輕物件被錯誤回收，造成 use-after-free |
| **Reproducibility (Reproducibility)** | `Low` | 需精確時序，單執行緒無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeRefMut::drop`, `cell.rs:1278-1299`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 `GcThreadSafeRefMut` 被 drop 時，如果 barrier 在任何時刻處於 active 狀態，應該捕獲並標記 GC 指針，確保這些指標指向的物件不會被錯誤回收。

### 實際行為 (Actual Behavior)
程式碼有兩次檢查 barrier 狀態：
1. 第一次檢查 (line 1281-1282): `barrier_active_at_start`
2. 第二次檢查 (line 1288-1289): `barrier_active_before_mark`

**問題**：在第二次檢查 (`barrier_active_before_mark`) 和實際標記 (`mark_object_black`) 之間，barrier 狀態可能從 INACTIVE 變為 ACTIVE。這導致：
- 如果 barrier 在第二次檢查時為 INACTIVE，但在標記前變為 ACTIVE，物件不會被標記
- 可能導致年輕物件在 minor GC 時被錯誤回收

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/cell.rs:1278-1299`:

```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        // 第一次檢查
        let barrier_active_at_start = crate::gc::incremental::is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active();

        let mut ptrs = Vec::with_capacity(32);
        (*self.inner).capture_gc_ptrs_into(&mut ptrs);

        // 第二次檢查
        let barrier_active_before_mark = crate::gc::incremental::is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active();

        if barrier_active_at_start || barrier_active_before_mark {
            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}
```

**Race Condition 時序**:
1. Thread A (mutator): 執行 drop，進行第二次檢查 → 結果為 INACTIVE
2. Thread B (collector): 啟動 incremental marking，barrier 變為 ACTIVE
3. Thread A: 執行標記，但因為 `barrier_active_before_mark` 為 false，不會進入標記邏輯
4. 年輕物件被錯誤回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒環境觸發 TOCTOU
// 此 bug 難以在單執行緒環境重現
// 建議使用 ThreadSanitizer 或設計特定時序的 stress test
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在標記前新增第三次檢查：

```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        let barrier_active_at_start = crate::gc::incremental::is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active();

        let mut ptrs = Vec::with_capacity(32);
        (*self.inner).capture_gc_ptrs_into(&mut ptrs);

        let barrier_active_before_mark = crate::gc::incremental::is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active();

        // 新增：標記前的最終檢查
        let barrier_active_now = crate::gc::incremental::is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active();

        if barrier_active_at_start || barrier_active_before_mark || barrier_active_now {
            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}
```

或者更簡單的做法：在 drop 時直接標記（接受輕微效能損失以換取安全）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU (Time-of-Check to Time-of-Use) race condition。在 incremental marking 系統中，mutator 和 collector 並發執行時，任何在檢查和操作之間的時間窗口都可能導致不一致的狀態。建議採用「樂觀標記」策略：只要有一次檢查為 true，就執行標記；或使用更強的同步機制。

**Rustacean (Soundness 觀點):**
這不會導致明確的 UB（因為檢查失敗只是不標記，不會訪問無效記憶體），但會導致記憶體安全問題：物件被錯誤回收後可能被重用，導致 use-after-free。

**Geohot (Exploit 觀點):**
雖然這是並發 bug，但攻擊者可以透過觸發 GC 請求來控制 timing，精確地在第二次檢查後、標記前啟動 incremental marking，實現 memory corruption。
