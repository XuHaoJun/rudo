# [Bug]: GcThreadSafeRefMut::drop 缺少 Generational Barrier 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 當 Generational GC 模式啟用但 Incremental Marking 關閉時觸發 |
| **Severity (嚴重程度)** | `High` | 導致 Young 物件被錯誤回收，造成記憶體安全問題 |
| **Reproducibility (復現難度)** | `Medium` | 需要僅啟用generational barrier的場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeRefMut::drop`, `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcThreadSafeRefMut` 的 Drop 實作只檢查 `is_incremental_marking_active()`，但缺少 `is_generational_barrier_active()` 檢查。這與 `GcRwLockWriteGuard` 和 `GcMutexGuard` 的 Drop 實作不一致，後兩者都正確檢查了兩種 barrier。

### 預期行為 (Expected Behavior)
當 Generational Barrier 啟用時（無論 Incremental Marking 是否啟用），Drop 應該捕獲並標記 GC 指針。

### 實際行為 (Actual Behavior)
當只有 Generational Barrier 啟用（Incremental Marking 關閉）時，`GcThreadSafeRefMut::drop` 不會捕獲 GC 指針，導致：
1. OLD→YOUNG 引用不被記錄
2. Young 物件可能在 Minor GC 時被錯誤回收
3. 記憶體安全問題

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/cell.rs:1169-1182`:

```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        // BUG: 只檢查 incremental marking，缺少 generational barrier 檢查！
        if crate::gc::incremental::is_incremental_marking_active() {
            let mut ptrs = Vec::with_capacity(32);
            (*self.inner).capture_gc_ptrs_into(&mut ptrs);

            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}
```

對比 `GcRwLockWriteGuard::drop` (sync.rs:372-385):
```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // 正確：檢查兩種 barrier
        if is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active()
        {
            // ... capture and mark
        }
    }
}
```

`trigger_write_barrier` 方法正確檢查了兩種 barrier (cell.rs:1024-1035)，但 drop 實作遺漏了。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 關閉 Incremental Marking，只啟用 Generational Barrier
2. 創建 `Gc<GcThreadSafeCell<Gc<T>>>` 
3. 建立 OLD→YOUNG 引用
4. 執行 Minor GC (`collect()`)
5. 嘗試訪問 Young 物件

預期：Young 物件應該存活（因為 OLD→YOUNG 引用被 barrier 記錄）
實際：Young 物件被錯誤回收（因為 barrier 沒有在 drop 時觸發）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `GcThreadSafeRefMut::drop` 以檢查兩種 barrier：

```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        if crate::gc::incremental::is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active()
        {
            let mut ptrs = Vec::with_capacity(32);
            (*self.inner).capture_gc_ptrs_into(&mut ptrs);

            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generational barrier 的核心目的是記錄 OLD→YOUNG 引用。如果 drop 時不捕獲 GC 指針，minor GC 將無法掃描到這些引用，導致 young 物件被錯誤回收。這是 generational GC 的基本正確性問題。

**Rustacean (Soundness 觀點):**
這導致記憶體不安全：被錯誤回收的物件記憶體可能被重新分配，後續存取會造成 use-after-free。

**Geohot (Exploit 觀點):**
攻擊者可以觸發此 bug 來實現記憶體佈局控制，進一步利用 use-after-free。

---

## ✅ 驗證記錄 (Verification Record)

**驗證日期:** 2026-02-25
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `crates/rudo-gc/src/cell.rs:1169-1182`:

1. `GcThreadSafeRefMut::drop` 只檢查 `is_incremental_marking_active()`
2. 對比 `GcRwLockWriteGuard::drop` 和 `GcMutexGuard::drop` 都正確檢查了兩種 barrier
3. `trigger_write_barrier` 方法正確檢查了兩種 barrier
4. 此不一致導致generational barrier-only 模式下 GC 行為不正確

---

## Resolution (2026-03-13)

**Outcome:** Fixed and verified.

### Code Changes

- Updated `GcThreadSafeRefMut::drop` in `cell.rs` to mirror the fix applied for bug107 (GcRwLockWriteGuard/GcMutexGuard):
  - Cache `incremental_active = is_incremental_marking_active()` and `generational_active = is_generational_barrier_active()` at the start of `drop`.
  - Call `mark_object_black` for captured pointers only when `incremental_active` is true (SATB path).
  - Call `unified_write_barrier(ptr, incremental_active)` when `generational_active` is true, using the raw pointer to the inner value (`&*self.inner`) as the barrier address.

This ensures OLD→YOUNG references established while holding the guard are recorded in the remembered set even when incremental marking is disabled but generational GC is active.

### Verification

- Added `test_gc_thread_safe_ref_mut_drop_triggers_generational_barrier` in `crates/rudo-gc/tests/gc_thread_safe_cell.rs`.
- Ran the targeted test and full suite: all passed.
