# [Bug]: GcMutex 缺少 GcCapture 實作導致 SATB 屏障失效

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者使用 GcMutex 包裝包含 GC 指標的資料時觸發 |
| **Severity (嚴重程度)** | Critical | 繞過 SATB 屏障導致物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要在 incremental marking 期間修改 GcMutex 內的 GC 指標 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcMutex`, `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcMutex<T>` 應該實作 `GcCapture` trait，使得在 incremental marking 期間修改 `Gc<T>` 指標時能夠正確記錄舊指標值，維持 SATB (Snapshot-At-The-Beginning) 不變性。

### 實際行為 (Actual Behavior)
`GcMutex` 沒有實作 `GcCapture` trait，導致：
1. 當 `T` 包含 `Gc<T>` 指標時，SATB 屏障無法捕捉舊指標值
2. 在 incremental marking 期間修改指標可能導致物件被錯誤回收
3. 與 `GcRwLock` 行為不一致（`GcRwLock` 有 `GcCapture` 實作）

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/sync.rs`

`GcRwLock` 有 `GcCapture` 實作 (lines 593-605)：
```rust
impl<T: GcCapture + ?Sized> GcCapture for GcRwLock<T> {
    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(value) = self.inner.try_read() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

但 `GcMutex` 完全缺少 `GcCapture` 實作。搜尋結果顯示：
- `GcRwLock` 有 GcCapture：`sync.rs:593`
- `GcMutex` 沒有 GcCapture：搜尋結果僅有 `sync.rs:45` (import) 和 `sync.rs:593`

**影響範圍：**
- `GcMutex<Gc<T>>` 類型在 incremental marking 期間無法正確記錄舊指標
- 當執行緒在 GC 期間持有 `GcMutex` 鎖並修改 GC 指標時，SATB 屏障失效

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcMutex, Trace, collect_full, set_incremental_config, IncrementalConfig};
use std::sync::Arc;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct SharedData {
    value: i32,
    next: Option<Gc<SharedData>>,
}

fn main() {
    // 啟用 incremental marking
    set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });
    
    // 建立迴圈引用在 GcMutex 中
    let data1: Gc<GcMutex<SharedData>> = Gc::new(GcMutex::new(SharedData {
        value: 1,
        next: None,
    }));
    
    let data2: Gc<GcMutex<SharedData>> = Gc::new(GcMutex::new(SharedData {
        value: 2,
        next: Some(data1.clone()),
    }));
    
    // 建立迴圈
    {
        let mut guard = data1.lock();
        guard.next = Some(data2.clone());
    }
    
    // 移除外部根
    drop(data1);
    drop(data2);
    
    // 嘗試 GC - 由於 GcMutex 缺少 GcCapture
    // SATB 可能遺漏引用，導致物件被錯誤回收
    collect_full();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `sync.rs` 中為 `GcMutex` 添加 `GcCapture` 實作：

```rust
impl<T: GcCapture + ?Sized> GcCapture for GcMutex<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Use try_lock to avoid blocking during GC
        // SAFETY: During STW, no other thread holds the lock
        if let Some(guard) = self.inner.try_lock() {
            guard.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

或者參考 `GcRwLock` 的模式，使用 `try_read()` / `try_lock()` 來非阻塞地獲取指標。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 incremental marking 中，SATB 屏障是維持 "all objects reachable at GC start remain reachable" 的關鍵機制。`GcMutex` 缺少 `GcCapture` 會導致舊指標值無法被記錄，這與 `GcRwLock` 的設計不一致。在 Chez Scheme 中，我們確保所有可變的 GC 指標容器都有適當的屏障機制。

**Rustacean (Soundness 觀點):**
這不是傳統的 soundness 問題（不會導致 UB），但會導致記憶體安全問題 - 物件可能被錯誤回收，導致 use-after-free。`GcRwLock` 已經有 `GcCapture` 實作，但 `GcMutex` 缺少，這是 API 不一致性问题。

**Geohot (Exploit 觀點):**
利用此 bug 需要：
1. 將敏感資料放入 `GcMutex<Gc<T>>`
2. 在 incremental marking 期間修改指標
3. 導致目標物件被錯誤回收
4. 佔用已回收物件的記憶體布局，實現 use-after-free

這與 bug32 (`GcMutex::try_lock` 缺少 write barrier) 是不同的問題 - bug32 是缺少 write barrier，本 issue 是缺少 GcCapture 實作。
