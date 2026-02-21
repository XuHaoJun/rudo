# [Bug]: GcMutex::try_lock() 缺少 Write Barrier 導致 SATB 不變性破壞

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 程式碼中若使用 `try_lock()` 而非 `lock()` 來取得鎖，會觸發此問題 |
| **Severity (嚴重程度)** | Critical | 繞過 write barrier 會破壞 SATB 不變性，導致物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要在 GC 期間使用 `try_lock()` 修改物件，穩定重現需要特定時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcMutex::try_lock`, `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcMutex::try_lock()` 應該與 `GcMutex::lock()` 行為一致，在成功取得鎖時觸發 generational/incremental write barrier，確保 SATB (Snapshot-At-The-Beginning) 不變性。

### 實際行為 (Actual Behavior)
`GcMutex::try_lock()` 完全沒有調用 `trigger_write_barrier()`，而 `GcMutex::lock()` 會在取得鎖前觸發 write barrier。這導致使用 `try_lock()` 時繞過了 write barrier 機制。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/sync.rs` 中：

**`GcMutex::lock()` (lines 460-467):**
```rust
pub fn lock(&self) -> GcMutexGuard<'_, T> {
    self.trigger_write_barrier();  // ✓ 調用了 write barrier
    let guard = self.inner.lock();
    GcMutexGuard {
        guard,
        _marker: PhantomData,
    }
}
```

**`GcMutex::try_lock()` (lines 489-494):**
```rust
pub fn try_lock(&self) -> Option<GcMutexGuard<'_, T>> {
    self.inner.try_lock().map(|guard| GcMutexGuard {
        guard,
        _marker: PhantomData,
    })
    // ✗ 沒有調用 trigger_write_barrier()!
}
```

對比 `GcRwLock::try_write()` (lines 248-256)，它正確地在成功取得鎖後調用了 write barrier：
```rust
pub fn try_write(&self) -> Option<GcRwLockWriteGuard<'_, T>> {
    self.inner.try_write().map(|guard| {
        self.trigger_write_barrier();  // ✓ 正確觸發 barrier
        GcRwLockWriteGuard {
            guard,
            _marker: PhantomData,
        }
    })
}
```

**影響範圍：**
- 當使用 `try_lock()` 成功取得 `GcMutex` 的鎖時
- 如果此時處於 incremental marking 或 generational GC 期間
- 對物件的修改不會被記錄到 SATB buffer
- 可能導致被標記為"dead"但實際仍被引用的物件被錯誤回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcMutex, Trace, collect_full};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct SharedData {
    value: i32,
    // 加入指標欄位使其更容易被錯誤回收
    nested: Option<Gc<SharedData>>,
}

fn main() {
    // 啟用 incremental marking 或 generational GC
    rudo_gc::set_incremental_config(rudo_gc::IncrementalConfig::default());
    
    // 建立迴圈引用
    let data1: Gc<GcMutex<SharedData>> = Gc::new(GcMutex::new(SharedData {
        value: 1,
        nested: None,
    }));
    
    let data2: Gc<GcMutex<SharedData>> = Gc::new(GcMutex::new(SharedData {
        value: 2,
        nested: Some(data1.clone()),
    }));
    
    // 建立迴圈
    if let Some(mut guard) = data1.try_lock() {
        guard.nested = Some(data2.clone());
    }
    
    // 移除外部根
    drop(data1);
    drop(data2);
    
    // 嘗試 GC - 由於 try_lock 沒有觸發 write barrier
    // SATB 可能遺漏引用，導致物件被錯誤回收
    collect_full();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `crates/rudo-gc/src/sync.rs` 的 `GcMutex::try_lock()` 方法中添加 write barrier：

```rust
pub fn try_lock(&self) -> Option<GcMutexGuard<'_, T>> {
    self.inner.try_lock().map(|guard| {
        self.trigger_write_barrier();  // 添加這行
        GcMutexGuard {
            guard,
            _marker: PhantomData,
        }
    })
}
```

或者參考 `GcRwLock::try_write()` 的模式，將 barrier 調用放在 map closure 內部。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 破壞了 SATB 不變性，是記憶體安全問題。在 incremental marking 期間，write barrier 是用來維護 "all objects alive at GC start remain reachable" 的關鍵機制。繞過 barrier 會導致 marked-as-dead 物件實際仍被引用，造成 use-after-free。

**Rustacean (Soundness 觀點):**
這是 API 不一致的問題。`try_lock()` 應該是 `lock()` 的非阻塞版本，但兩者行為不一致。從 Soundness 角度，這不直接是 UB，但會導致執行期記憶體錯誤。

**Geohot (Exploit 觀點):**
利用此 bug 需要控制 GC 時序。攻擊者可以：
1. 使用 `try_lock()` 建立一時序窗口
2. 在 GC 標記期間快速修改物件
3. 導致目標物件被錯誤回收
4. 佔用已回收物件的記憶體布局，實現 use-after-free

---

## ✅ 確認記錄 (Confirmation Record)

**Date:** 2026-02-21
**Confirmed by:** Bug hunt analysis

程式碼確認：`sync.rs:489-494` 中的 `try_lock()` 方法仍然缺少 `trigger_write_barrier()` 調用。與 `GcRwLock::try_write()` (sync.rs:248-256) 的正確實現形成對比。

**Verification:** 
- `GcMutex::lock()` at line 461 correctly calls `self.trigger_write_barrier()`
- `GcMutex::try_lock()` at line 489-494 does NOT call `trigger_write_barrier()`
- `GcRwLock::try_write()` at line 250 correctly calls `self.trigger_write_barrier()`
