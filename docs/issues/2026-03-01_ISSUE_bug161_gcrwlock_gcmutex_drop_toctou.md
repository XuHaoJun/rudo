# [Bug]: GcRwLockWriteGuard 與 GcMutexGuard Drop TOCTOU 導致 Barrier 遺漏

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需多執行緒：mutator drop 與 collector 啟動 incremental marking 的時序交錯 |
| **Severity (嚴重程度)** | `High` | 導致年輕物件被錯誤回收，造成 use-after-free |
| **Reproducibility (Reproducibility)** | `Low` | 需精確時序，單執行緒無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop`, `GcMutexGuard::drop` (sync.rs:407-428, sync.rs:656-677)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 `GcRwLockWriteGuard` 或 `GcMutexGuard` 被 drop 時，如果 barrier 在任何時刻處於 active 狀態，應該捕獲並標記 GC 指針，確保這些指標指向的物件不會被錯誤回收。

### 實際行為 (Actual Behavior)
程式碼有兩次檢查 barrier 狀態：
1. 第一次檢查 (line 410-411): `barrier_active_at_start`
2. 第二次檢查 (line 417-418): `barrier_active_before_mark`

**問題**：在第二次檢查 (`barrier_active_before_mark`) 和實際標記 (`mark_object_black`) 之間，barrier 狀態可能從 INACTIVE 變為 ACTIVE。這導致：
- 如果 barrier 在第二次檢查時為 INACTIVE，但在標記前變為 ACTIVE，物件不會被標記
- 可能導致年輕物件在 minor GC 時被錯誤回收

此問題與 bug160 (GcThreadSafeRefMut::drop TOCTOU) 相同，但發生在不同的組件。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/sync.rs:407-428` (GcRwLockWriteGuard) 和 `crates/rudo-gc/src/sync.rs:656-677` (GcMutexGuard):

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // 第一次檢查
        let barrier_active_at_start =
            is_generational_barrier_active() || is_incremental_marking_active();

        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        // 第二次檢查
        let barrier_active_before_mark =
            is_generational_barrier_active() || is_incremental_marking_active();

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

在標記前新增第三次檢查（與 bug160 相同的修復）：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let barrier_active_at_start =
            is_generational_barrier_active() || is_incremental_marking_active();

        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        let barrier_active_before_mark =
            is_generational_barrier_active() || is_incremental_marking_active();

        // 新增：標記前的最終檢查
        let barrier_active_now =
            is_generational_barrier_active() || is_incremental_marking_active();

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
此 TOCTOU 問題會導致 SATB barrier 失效，特別是在增量標記期間。當 mutator 在增量標記啟動時釋放鎖，卻未能標記其持有的 GC 指針時，這些指標指向的年輕物件可能會被錯誤回收。這與 bug160 相同的模式，但發生在不同的同步原語中。

**Rustacean (Soundness 觀點):**
這是一個內存安全問題。如果年輕物件被錯誤回收，後續存取這些指標會導致 use-after-free。這是 Rust 中未定義行為的一種形式。

**Geohot (Exploit 觀點):**
雖然這個 bug 很難利用（需要精確的時序控制），但理論上攻擊者可以通過構造特定的執行緒調度來觸發 use-after-free。建議修復此問題以消除這個攻擊面。

---

## Resolution (2026-03-02)

**Outcome:** Fixed.

**Fix:** Applied the same pattern as bug160 (GcThreadSafeRefMut::drop): eliminated TOCTOU by always marking when we have captured ptrs, instead of checking barrier state before marking. The check-then-mark pattern had a race window between the second check and the actual mark. `mark_object_black` is idempotent and safe when barrier is inactive, so always marking removes the race entirely. Updated both `GcRwLockWriteGuard::drop` and `GcMutexGuard::drop` in `sync.rs`.

