# [Bug]: GcRwLockWriteGuard/GcMutexGuard Drop 缺少 Generational Barrier 檢查

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當用戶啟用generational GC但禁用incremental marking時會觸發 |
| **Severity (嚴重程度)** | High | 會導致 OLD→YOUNG 引用在minor collection時被遺漏，造成記憶體不安全 |
| **Reproducibility (復現難度)** | Medium | 需要配置generational GC + 禁用incremental marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop()`, `GcMutexGuard::drop()` in `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcRwLockWriteGuard` 和 `GcMutexGuard` 的 Drop 實作只檢查 `is_incremental_marking_active()`，但**沒有檢查 `is_generational_barrier_active()`**。

這導致當：
- Incremental marking 被禁用
- 但 Generational GC 被啟用

在此情況下，Drop 時不會觸發任何 barrier，導致 OLD→YOUNG 引用被遺漏，minor collection 可能錯誤回收 young 物件。

### 預期行為
Drop 時應該檢查 `is_generational_barrier_active() || is_incremental_marking_active()`，與 `GcRwLock::write()` 和 `GcMutex::lock()` 的 barrier 觸發邏輯一致。

### 實際行為
Drop 只檢查 `is_incremental_marking_active()`，當 incremental marking 禁用但 generational GC 啟用時，不會觸發任何 barrier。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. `GcRwLock::trigger_write_barrier()` 正確地檢查：`is_generational_barrier_active() || is_incremental_marking_active()`
2. `GcMutex::trigger_write_barrier()` 正確地檢查：`is_generational_barrier_active() || is_incremental_marking_active()`
3. **但** `GcRwLockWriteGuard::drop()` 只檢查：`is_incremental_marking_active()`
4. **且** `GcMutexGuard::drop()` 只檢查：`is_incremental_marking_active()`

這是不一致的行為。當取得 lock 時，會觸發 generational barrier，但當 drop 時反而沒有，導致在 lock 持有期間的修改不會被追蹤。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 配置只啟用 generational GC（禁用 incremental marking）
2. 創建 `Gc<GcRwLock<T>>` 或 `Gc<GcMutex<T>>`
3. 獲取 write lock
4. 写入一个年轻代 GC 指针
5. 释放 write lock (drop guard)
6. 触发 minor collection
7. 年轻代对象可能被错误回收

```rust
// PoC 概念验证
fn test_generational_barrier_on_drop() {
    // 配置：启用 generational GC，禁用 incremental marking
    // ...
    
    let data: Gc<GcRwLock<SharedData>> = Gc::new(GcRwLock::new(SharedData {
        young_ref: None,
    }));
    
    // 创健一个年轻代对象
    let young = Gc::new(YoungData { value: 42 });
    
    // 获取 write lock
    {
        let mut guard = data.write();
        guard.young_ref = Some(young);  // OLD -> YOUNG 引用
    } // Drop 时应该触发 generational barrier，但没有！
    
    // Minor collection
    collect();  // 年轻代对象可能被错误回收
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

修改 `sync.rs` 中的 Drop 實作：

```rust
// GcRwLockWriteGuard::drop()
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // 應該檢查generational barrier，不只是incremental marking
        if crate::gc::incremental::is_incremental_marking_active() {
            let mut ptrs = Vec::with_capacity(32);
            self.guard.capture_gc_ptrs_into(&mut ptrs);
            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
        // 或者：觸發 unified_write_barrier 來處理generational barrier
    }
}

// GcMutexGuard::drop() - 同樣需要修復
```

更好的方案是使用與 `trigger_write_barrier()` 相同的模式：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();
        
        if incremental_active || generational_active {
            let mut ptrs = Vec::with_capacity(32);
            self.guard.capture_gc_ptrs_into(&mut ptrs);
            
            if incremental_active {
                for gc_ptr in ptrs {
                    let _ = unsafe {
                        crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                    };
                }
            }
            // 或者對每個ptr調用 generational barrier
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generational GC 的核心原則是 OLD→YOUNG 引用必須被追蹤，無論是incremental marking期間還是在一般的generational collection期間。如果在lock drop時不觸發generational barrier，會破壞這個不變性，導致minor collection可能錯誤回收young物件。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當OLD物件持有對YOUNG物件的引用時，必須確保這個引用被記錄，否則YOUNG物件可能被回收，導致use-after-free。

**Geohot (Exploit 觀點):**
攻擊者可以利用這個bug，在禁用incremental marking的情況下，通過構造特定的引用模式來導致use-after-free漏洞。

---

## 相關 Bug

- bug18/bug59: GcRwLockWriteGuard/GcMutexGuard Drop 缺少 SATB Barrier（已修復）
- bug98: is_generational_barrier_active() returns false when incremental marking disabled

---

## Resolution Note (2026-02-26)

**Classification: Invalid** — The fix is already implemented. Both `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()` in `sync.rs` (lines 408–411 and 650–653) already check `is_generational_barrier_active() || is_incremental_marking_active()` before capturing and marking GC pointers. The behavior matches `trigger_write_barrier()`. No code changes required.

---

## Reopening Note (2026-03-09)

**REOPENED - Bug is still present!**

The resolution above was INCORRECT. Upon re-examination:

**Root Cause - Wrong function called:**

1. `GcRwLockReadGuard::drop` (sync.rs:386-396) - Calls `mark_object_black` only
2. `GcRwLockWriteGuard::drop` (sync.rs:436-449) - Calls `mark_object_black` only
3. `GcMutexGuard::drop` (sync.rs:691-704) - Calls `mark_object_black` only

All three Drop implementations:
- Capture GC pointers via `capture_gc_ptrs_into` ✓
- Call `mark_object_black` which handles **incremental marking** (SATB) only
- Do NOT call `unified_write_barrier` which handles **generational barrier** (remembered set)

Compare to acquisition (e.g., `GcRwLock::write()` at line 256-270):
- Calls `record_satb_old_values_with_state` for SATB ✓
- Calls `trigger_write_barrier_with_state(generational_active, incremental_active)` 
- Which calls `unified_write_barrier` for generational barrier ✓

The Drop implementations are calling the WRONG function:
- `mark_object_black` = incremental marking only (SATB)
- `unified_write_barrier` = generational barrier (remembered set for OLD→YOUNG)

Even if incremental marking is disabled but generational GC is enabled, the Drop should still record pointers in the remembered set via `unified_write_barrier`!

**The bug109 (GcThreadSafeRefMut) was also marked "Fixed" but the same bug is still present in cell.rs:1276-1288.**
