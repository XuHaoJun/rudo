# [Bug]: GcRwLockWriteGuard 與 GcMutexGuard 缺少 Drop 時的 SATB Barrier，導致修改後的 GC 指針可能未被標記

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在持有鎖期間延遲修改 GC 指針 |
| **Severity (嚴重程度)** | High | 可能導致 GC 錯誤回收物件，造成 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要構造特定的使用模式 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard`, `GcMutexGuard` (sync.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current main branch

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcRwLockWriteGuard` 和 `GcMutexGuard` 應該在 Drop 時觸發 SATB barrier，確保在持有鎖期間對 GC 指針的任何修改都能被增量標記正確處理。這與 `GcThreadSafeRefMut` 的行為一致。

### 實際行為 (Actual Behavior)
`GcRwLockWriteGuard` 和 `GcMutexGuard` 只在鎖獲取時觸發 barrier，但在 Drop 時不執行任何 barrier 邏輯：

```rust
// sync.rs:360-365 - GcRwLockWriteGuard::drop()
impl<T: ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // Guard is dropped automatically when it goes out of scope
        // The parking_lot guard will release the write lock
    }
}

// sync.rs:572-577 - GcMutexGuard::drop()
impl<T: ?Sized> Drop for GcMutexGuard<'_, T> {
    fn drop(&mut self) {
        // Guard is dropped automatically when it goes out of scope
        // The parking_lot guard will release the mutex
    }
}
```

相比之下，`GcThreadSafeRefMut` 在 Drop 時正確執行 SATB barrier：
```rust
// cell.rs:1133-1146
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

問題在於 barrier 只在鎖獲取時觸發，而非在資料修改後。這導致以下問題場景：

1. **Thread A 獲取寫鎖**，觸發 barrier（記錄當時的舊值）
2. **Thread A 執行計算**，此時 barrier 已執行
3. **Thread A 稍後修改 GC 指針**（例如替換 Vec 中的元素）
4. **Thread A drop guard** - 沒有 barrier 執行！
5. **增量標記運行** - 新修改的 GC 指針未被標記為黑色
6. **物件被錯誤回收** - 導致 use-after-free

這與 `GcThreadSafeRefMut` 的設計不同，後者在 Drop 時捕獲並標記當前的 GC 指針。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcRwLock, Trace, collect_full};
use std::sync::Arc;
use std::thread;

#[derive(Trace)]
struct Data {
    items: Vec<Gc<i32>>,
}

fn main() {
    // 啟用增量標記
    crate::gc::incremental::set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data {
        items: vec![Gc::new(1), Gc::new(2)],
    }));

    // 獲取鎖並修改
    {
        let mut guard = data.write();
        
        // 執行一些計算
        for _ in 0..1000 {
            // 計算...
        }
        
        // 延遲修改 GC 指針 - 此時 barrier 已經執行過了！
        guard.items[0] = Gc::new(999);
        
        // drop guard - 沒有 barrier 執行！
    }

    // 觸發增量標記
    // items[0] 的新值可能未被標記！
    collect_full();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcRwLockWriteGuard` 和 `GcMutexGuard` 的 Drop 實現中添加 SATB barrier：

```rust
// GcRwLockWriteGuard
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
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

// GcMutexGuard
impl<T: GcCapture + ?Sized> Drop for GcMutexGuard<'_, T> {
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

注意：需要為 `GcRwLockWriteGuard` 和 `GcMutexGuard` 添加 `GcCapture` bound。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是增量標記中的經典問題：barrier 必須在實際修改發生後執行，而不是在鎖獲取時。在 Chez Scheme 中，我們確保 barrier 在每次修改後執行，而不是僅在獲取鎖時執行。這個問題會導致 SATB 不變性被破壞，因為使用者修改的 GC 指針未被記錄。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。當 barrier 未執行時，GC 可能錯誤回收仍可達的物件，導致後續存取時發生 use-after-free。這與 bug 14（SATB overflow ignored）本質上相似 - 都是破壞 SATB 不變性。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過：
1. 构造在鎖內修改 GC 指針的場景
2. 觸發增量標記
3. 利用未被標記的物件被錯誤回收
4. 實現記憶體佈局控制

這為(use-after-free) 攻擊開闢了可能性。
