# [Bug]: GcRwLockWriteGuard 與 GcMutexGuard Drop 時缺少 SATB Barrier 標記

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 incremental marking 期間使用 GcRwLock/GcMutex 進行寫入 |
| **Severity (嚴重程度)** | High | 可能導致新写入的 GC 指標物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要特定條件：incremental marking + write + drop |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop()`, `GcMutexGuard::drop()` (`sync.rs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcRwLockWriteGuard` 和 `GcMutexGuard` 在 Drop 時，應該在 incremental marking 期間觸發 SATB barrier 標記，即將新寫入的 GC 指標物件標記為黑色（alive），確保它們不會被錯誤回收。

### 實際行為 (Actual Behavior)
目前這兩個類型的 `drop()` 實作是空的，沒有執行任何 SATB barrier 標記：

```rust
// sync.rs:360-365
impl<T: ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // Guard is dropped automatically when it goes out of scope
        // The parking_lot guard will release the write lock
    }
}

// sync.rs:572-577
impl<T: ?Sized> Drop for GcMutexGuard<'_, T> {
    fn drop(&mut self) {
        // Guard is dropped automatically when it goes out of scope
        // The parking_lot guard will release the mutex
    }
}
```

### 對比：GcThreadSafeRefMut 的正確實作

`GcThreadSafeRefMut` 正確地在 drop 時執行 SATB barrier 標記：

```rust
// cell.rs:1007-1020
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
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

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `sync.rs` 中，`GcRwLockWriteGuard` 和 `GcMutexGuard` 的 Drop 實作只釋放了鎖，但沒有執行 SATB barrier 標記。

問題在於：
1. 當使用 `GcRwLock::write()` 或 `GcMutex::lock()` 獲得寫入權限時，新寫入的 GC 指標需要被記錄
2. `GcRwLock::write()` 和 `GcMutex::lock()` 會在獲得鎖後觸發 write barrier
3. 但是當 guard drop 時，如果此時正在進行 incremental marking，新寫入的物件應該被標記為黑色（alive）
4. 目前的實作缺少這個 drop 時的 barrier 觸發

這與 `GcThreadSafeRefMut` 的行為不一致，後者正確地實現了這個功能。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcRwLock, Trace, collect_full, gc::incremental::IncrementalConfig};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[derive(Trace)]
struct Container {
    lock: GcRwLock<Data>,
}

fn main() {
    // 啟用 incremental marking
    let config = IncrementalConfig {
        enabled: true,
        ..Default::default()
    };
    rudo_gc::gc::incremental::set_incremental_config(config);

    let gc = Gc::new(Container {
        lock: GcRwLock::new(Data { value: 0 }),
    });

    // 觸發一次 major GC 進入 incremental marking
    collect_full();
    
    // 使用 GcRwLock 進行寫入
    {
        let mut guard = gc.lock.write();
        guard.value = 42;
        // guard 在此處 drop，應該觸發 SATB barrier
    }
    
    // 預期：新寫入的 Data 物件應該被標記為 alive
    // 實際：可能因為缺少 drop barrier 而被錯誤回收
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `sync.rs` 中為 `GcRwLockWriteGuard` 和 `GcMutexGuard` 添加正確的 Drop 實作：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        if crate::gc::incremental::is_incremental_marking_active() {
            let mut ptrs = Vec::with_capacity(32);
            (*self.guard).capture_gc_ptrs_into(&mut ptrs);

            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}

impl<T: GcCapture + ?Sized> Drop for GcMutexGuard<'_, T> {
    fn drop(&mut self) {
        if crate::gc::incremental::is_incremental_marking_active() {
            let mut ptrs = Vec::with_capacity(32);
            (*self.guard).capture_gc_ptrs_into(&mut ptrs);

            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}
```

注意：`GcRwLock<T>` 和 `GcMutex<T>` 都已經實現了 `GcCapture` trait（見 `sync.rs:593-605`），所以可以使用 `capture_gc_ptrs_into()` 方法。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 incremental marking 中，SATB barrier 的核心原則是：一旦物件在標記階段開始時是可達的，它就應該保持可達。新寫入的指標需要在 write barrier 或 drop barrier 中被記錄，否則可能被錯誤回收。這與 `GcThreadSafeRefMut` 的實現模式一致。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。如果新創建的 GC 物件在 incremental marking 期間被錯誤回收，後續對這些指標的解引用將導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個漏洞：
1. 觸發 incremental marking
2. 使用 GcRwLock/GcMutex 寫入新指標
3. 誘使 GC 錯誤回收這些物件
4. 通過 use-after-free 進行進一步利用

---

## 備註

此 bug 與 bug32 (`GcMutex::try_lock` missing barrier) 相關但不同：
- bug32: try_lock 缺少 acquire 時的 barrier
- 本 bug: guard drop 時缺少 SATB barrier 標記
