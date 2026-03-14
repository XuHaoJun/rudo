# [Bug]: GcRwLockWriteGuard/GcMutexGuard Drop barrier inconsistency - remembered buffer not updated when only incremental marking is active

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking 啟用但 generational barrier 未啟用的情況 |
| **Severity (嚴重程度)** | High | 可能導致 incremental marking 期間物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要minor GC + incremental marking 同時運行的特定場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop()`, `GcMutexGuard::drop()`, `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

在 `GcRwLockWriteGuard` 和 `GcMutexGuard` 的 Drop 實作中，當 incremental marking 啟用時，應該調用 `unified_write_barrier` 來更新 remembered buffer，確保 currently reachable 的物件被正確標記。

### 實際行為 (Actual Behavior)

在 Drop 實作中：
- 第 466-472 行：當 `incremental_active || generational_active` 時標記 GC 指針為黑色
- 第 474-477 行：僅當 `generational_active` 時調用 `unified_write_barrier`

這造成不一致：當只有 incremental marking 啟用（但 generational barrier 未啟用）時：
1. GC 指針被標記為黑色 ✓ (正確)
2. `unified_write_barrier` 未被調用 ✗ (錯誤 - 應該更新 remembered buffer)

### 程式碼位置

`sync.rs` 第 458-479 行 (`GcRwLockWriteGuard::drop`):

```rust
if incremental_active || generational_active {
    for gc_ptr in &ptrs {
        let _ = unsafe {
            crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
        };
    }
}

if generational_active {  // <-- BUG: 應該是 incremental_active || generational_active
    let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
    crate::heap::unified_write_barrier(ptr, incremental_active);
}
```

`sync.rs` 第 725-749 行 (`GcMutexGuard::drop`) - 相同問題

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於條件判斷不一致：

1. **標記黑色** (lines 466-472): 當 `incremental_active || generational_active` 時執行
2. **調用 barrier** (lines 474-477): 僅當 `generational_active` 時執行

當只有 incremental marking 啟用時，`unified_write_barrier` 不會被調用，導致：
- 頁面不會被加入 remembered buffer
- 在 incremental marking 期間，該頁面中的物件可能不會被視為 root
- 可能導致 live objects 被錯誤回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcRwLock, GcMutex, Trace};

// 需要 config 来啟用 incremental marking 但停用 generational barrier
// 這需要在特定配置下運行

#[derive(Trace)]
struct Data {
    gc_field: Gc<i32>,
}

fn test() {
    // Create a GcRwLock with GC pointer
    let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data {
        gc_field: Gc::new(42),
    }));
    
    // Enable incremental marking only (特定配置)
    // ...
    
    // Mutate through write guard
    {
        let mut guard = data.write();
        guard.gc_field = Gc::new(100);
    } // Drop triggers barrier
    
    // At this point, if incremental is active but not generational:
    // - The new object (100) was marked black
    // - But the page was NOT added to remembered buffer
    // - During incremental marking, the new object might be missed!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `GcRwLockWriteGuard::drop()` 和 `GcMutexGuard::drop()` 的條件：

```rust
// 錯誤 (current)
if generational_active {
    let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
    crate::heap::unified_write_barrier(ptr, incremental_active);
}

// 正確 (fix)
if generational_active || incremental_active {
    let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
    crate::heap::unified_write_barrier(ptr, incremental_active);
}
```

這樣確保當 incremental marking 啟用時，remembered buffer 也會被正確更新。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 incremental marking 期間，SATB 屏障需要記錄所有在 marking 開始時可達的物件。當前實現中，標記黑色和更新 remembered buffer 的邏輯不一致，可能導致在 incremental marking 期間丟失 root。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 UB，但可能導致 use-after-free - live objects 在 incremental marking 期間被錯誤回收。

**Geohot (Exploit 攻擊觀點):**
在特定條件下（incremental only mode），攻擊者可能能夠利用這個 window 來導致物件被錯誤回收，進而造成 use-after-free。

---
