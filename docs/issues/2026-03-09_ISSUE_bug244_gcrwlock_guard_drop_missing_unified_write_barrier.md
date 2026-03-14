# [Bug]: GcRwLockReadGuard/GcRwLockWriteGuard/GcMutexGuard Drop 僅調用 mark_object_black，缺少 unified_write_barrier 進行generational barrier

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當用戶啟用generational GC但禁用incremental marking時會觸發 |
| **Severity (嚴重程度)** | High | 會導致 OLD→YOUNG 引用在minor collection時被遺漏，造成記憶體不安全 |
| **Reproducibility (復現難度)** | Medium | 需要配置generational GC + 禁用incremental marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockReadGuard::drop()`, `GcRwLockWriteGuard::drop()`, `GcMutexGuard::drop()` in `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
當 guard drop 時，應該調用 `unified_write_barrier` 來處理 generational barrier（記住 OLD→YOUNG 引用），與 lock 獲取時的 barrier 行為一致。

### 實際行為

所有三個 Drop 實現僅調用 `mark_object_black`（處理 incremental marking/SATB），但**不調用** `unified_write_barrier`（處理 generational barrier/remembered set）。

**問題代碼位置:**
- `sync.rs:386-395` - `GcRwLockReadGuard::drop`
- `sync.rs:436-448` - `GcRwLockWriteGuard::drop`  
- `sync.rs:691-703` - `GcMutexGuard::drop`

**對比 lock 獲取 (正確實現):**
- `GcRwLock::write()` (sync.rs:256-270) 調用:
  - `record_satb_old_values_with_state` for SATB ✓
  - `trigger_write_barrier_with_state(generational_active, incremental_active)` 
  - 其中調用 `unified_write_barrier` 處理 generational barrier ✓

**Drop 實現 (錯誤):**
- 僅調用 `mark_object_black` = incremental marking only (SATB)
- 不調用 `unified_write_barrier` = generational barrier (remembered set for OLD→YOUNG)

這導致：當 incremental marking 禁用但 generational GC 啟用時，drop 時不會記錄任何 barrier，導致 OLD→YOUNG 引用被遺漏，minor collection 可能錯誤回收 young 物件。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. `mark_object_black` 函數 (incremental.rs:1002) 只處理 incremental marking 的 SATB barrier
2. `unified_write_barrier` 函數 (heap.rs:2916) 處理 generational barrier (remembered set)
3. Drop 實現錯誤地只調用 `mark_object_black`，沒有調用 `unified_write_barrier`

當用戶配置：
- Incremental marking = disabled
- Generational GC = enabled

在這種配置下：
- `is_incremental_marking_active()` = false
- `is_generational_barrier_active()` = true

但 Drop 只調用 `mark_object_black`（檢查 incremental marking），跳過了 generational barrier 處理。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 配置：啟用 generational GC，禁用 incremental marking
2. 創建 `Gc<GcRwLock<T>>` 或 `Gc<GcMutex<T>>`
3. 獲取 write lock
4. 写入一个年轻代 GC 指针 (OLD → YOUNG 引用)
5. 释放 write lock (drop guard) - **此時應該觸發 generational barrier，但沒有！**
6. 触发 minor collection
7. 年轻代对象可能被错误回收

```rust
fn test_generational_barrier_on_drop() {
    // 配置：啟用 generational GC，禁用 incremental marking
    set_incremental_config(IncrementalConfig {
        enabled: false,  // 禁用 incremental marking
        ..Default::default()
    });
    
    let data: Gc<GcRwLock<SharedData>> = Gc::new(GcRwLock::new(SharedData {
        young_ref: None,
    }));
    
    // 先 collect_full 將 data 提升到 old gen
    collect_full();
    
    // 創建年轻代对象
    let young = Gc::new(YoungData { value: 42 });
    
    // 獲取 write lock
    {
        let mut guard = data.write();
        guard.young_ref = Some(young);  // OLD -> YOUNG 引用
    } // Drop 時應該觸發 generational barrier，但只調用了 mark_object_black！
    
    // Minor collection - 年輕對象可能被錯誤回收
    collect();
    
    // 訪問 young 對象可能導致 use-after-free!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改所有三個 Drop 實現，添加對 generational barrier 的處理：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let incremental_active = is_incremental_marking_active();
        let generational_active = is_generational_barrier_active();
        
        if !incremental_active && !generational_active {
            return;
        }
        
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);
        
        for gc_ptr in ptrs {
            let ptr = gc_ptr.as_ptr() as *const u8;
            
            // For generational barrier: record in remembered set
            if generational_active {
                unsafe {
                    crate::heap::unified_write_barrier(ptr, incremental_active);
                }
            }
            
            // For incremental marking: mark black (SATB)
            if incremental_active {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(ptr)
                };
            }
        }
    }
}
```

同樣的修復應用於 `GcRwLockReadGuard::drop` 和 `GcMutexGuard::drop`。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generational GC 的核心原則是 OLD→YOUNG 引用必須被追蹤，無論是 incremental marking 期間還是在一般的 generational collection 期間。Drop 時不觸發 generational barrier 會破壞這個不變性，導致minor collection 可能錯誤回收 young 物件。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當 OLD 物件持有對 YOUNG 物件的引用時，必須確保這個引用被記錄在 remembered set 中，否則 YOUNG 物件可能被回收，導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以利用這個 bug，在禁用 incremental marking 的情況下，通過構造特定的 OLD→YOUNG 引用模式來導致 use-after-free 漏洞。

---

## 相關 Bug

- bug107: GcRwLockWriteGuard/GcMutexGuard Drop 缺少 Generational Barrier 檢查 (被錯誤標記為 Fixed，現已 Reopen)
- bug109: GcThreadSafeRefMut::drop 同樣問題（也只調用 mark_object_black）
- bug18/bug59: GcRwLockWriteGuard/GcMutexGuard Drop 缺少 SATB Barrier（已修復）

---

## 驗證記錄

**驗證日期:** 2026-03-09
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `sync.rs`:

1. `GcRwLockReadGuard::drop` (lines 386-395) - 僅調用 `mark_object_black`
2. `GcRwLockWriteGuard::drop` (lines 436-448) - 僅調用 `mark_object_black`
3. `GcMutexGuard::drop` (lines 691-703) - 僅調用 `mark_object_black`

對比 `GcRwLock::write()` (lines 256-270) 正確調用了 `trigger_write_barrier_with_state(generational_active, incremental_active)`。

問題根因：Drop 實現調用了錯誤的 barrier 函數。

---

## Resolution (2026-03-15)

**Outcome:** Already fixed.

The fix was applied in a prior commit. The current implementation in `sync.rs` correctly calls `unified_write_barrier` when `generational_active` in all three Drop implementations:

- `GcRwLockReadGuard::drop` (lines 426-430)
- `GcRwLockWriteGuard::drop` (lines 487-491)
- `GcMutexGuard::drop` (lines 758-762)

Each Drop now invokes both `mark_object_black` (SATB) and `unified_write_barrier` (generational remembered set) when the respective barriers are active. Verified by `test_gc_rwlock_write_guard_drop_triggers_generational_barrier` in `tests/sync.rs`.
