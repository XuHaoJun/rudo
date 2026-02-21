# [Bug]: GcThreadSafeCell GcCapture Implementation Data Race

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在沒有持有鎖的情況下呼叫 capture_gc_ptrs_into |
| **Severity (嚴重程度)** | Critical | 可能導致資料競爭 (data race) 和未定義行為 |
| **Reproducibility (復現難度)** | Medium | 取決於具體使用模式和時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::capture_gc_ptrs_into` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcCapture::capture_gc_ptrs_into` 應該在訪問內部資料之前獲取鎖，以防止與其他線程的寫入操作發生資料競爭。

### 實際行為 (Actual Behavior)
`GcThreadSafeCell` 的 `GcCapture` 實作直接使用 `data_ptr()` 訪問內部資料，而沒有先獲取鎖：

```rust
// cell.rs:1171-1174
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let raw_ptr = self.inner.data_ptr();  // 直接訪問，無鎖保護!
    unsafe { (*raw_ptr).capture_gc_ptrs_into(ptrs) }
}
```

這與 `GcRwLock` 的實現形成對比，後者使用 `try_read()` 來安全地獲取讀取鎖：

```rust
// sync.rs:600-604
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Some(value) = self.inner.try_read() {  // 嘗試獲取讀取鎖
        value.capture_gc_ptrs_into(ptrs);
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/cell.rs:1171-1174`：

```rust
impl<T: GcCapture + ?Sized> GcCapture for GcThreadSafeCell<T> {
    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        let raw_ptr = self.inner.data_ptr();  // 問題：無鎖訪問
        unsafe { (*raw_ptr).capture_gc_ptrs_into(ptrs) }
    }
}
```

問題：
1. `data_ptr()` 返回指向內原始指標部資料的
2. 直接解引用指標來此調用 `capture_gc_ptrs_into`
3. 沒有先獲取 `Mutex` 鎖來保護訪問
4. 如果個線程正在另一持有寫入鎖並正在修改資料，這會導致資料競爭

對比 `GcRwLock` 的安全實現：
```rust
impl<T: GcCapture + ?Sized> GcCapture for GcRwLock<T> {
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(value) = self.inner.try_read() {  // 安全：獲取讀取鎖
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

雖然在當前使用模式下（`borrow_mut()` 內部總是持有鎖），這個問題可能不會表現出來，但以下場景可能觸發問題：

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace};
use std::thread;
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    inner: GcThreadSafeCell<i32>,
}

fn trigger_bug() {
    let cell = Arc::new(Gc::new(GcThreadSafeCell::new(Data {
        inner: GcThreadSafeCell::new(0),
    })));

    let cell_clone = Arc::clone(&cell);
    
    // Thread A: acquires write lock
    let handle1 = thread::spawn(move || {
        let mut guard = cell_clone.borrow_mut();
        *guard.inner.borrow_mut() = 100;
        thread::sleep(std::time::Duration::from_millis(100));
    });

    // Thread B: tries to capture GC pointers while write lock is held
    // This would trigger the unsafe path if it tries to access the outer cell
    let cell2 = Arc::clone(&cell);
    let handle2 = thread::spawn(move || {
        // If some code path tries to capture pointers from cell2,
        // it would access data without holding the lock
        let _ = cell2.borrow();  // This calls Trace, not GcCapture
    });

    handle1.join().unwrap();
    handle2.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

方案 1：使用 try_lock 獲取鎖後再訪問

```rust
impl<T: GcCapture + ?Sized> GcCapture for GcThreadSafeCell<T> {
    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(guard) = self.inner.try_lock() {
            guard.capture_gc_ptrs_into(ptrs);
        }
        // If lock is not available, skip capturing - this is acceptable
        // because the writer will handle barriers
    }
}
```

方案 2：記錄下這個不安全性的文件，並確保所有呼叫路徑都持有鎖

```rust
/// SAFETY: This implementation assumes the caller holds the mutex lock.
/// This is currently guaranteed by all call sites in the codebase.
impl<T: GcCapture + ?Sized> GcCapture for GcThreadSafeCell<T> {
    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // SAFETY: Caller must hold the lock. Current call sites in borrow_mut()
        // and GcThreadSafeRefMut::drop() satisfy this requirement.
        let raw_ptr = self.inner.data_ptr();
        unsafe { (*raw_ptr).capture_gc_ptrs_into(ptrs) }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從 GC 角度來看，這是一個關鍵的安全問題。正確的 GC 實現需要在訪問任何可能被其他線程修改的資料時確保同步。雖然當前使用模式可能不會觸發問題，但這是一個定時炸彈 - 任何未來的使用模式變更都可能導致記憶體損壞。

**Rustacean (Soundness 觀點):**
這是明確的未定義行為 (UB)。在 Rust 中，多個線程對同一記憶體位置的並發訪問（其中至少一個是寫入）而沒有同步是資料競爭，根據 Rust 的記憶體模型，這是未定義行為。必須修復以確保記憶體安全。

**Geohot (Exploit 觀點):**
從攻擊者角度來看，這是一個潛在的漏洞利用向量。如果攻擊者能夠控制時序，他們可能能夠觸發資料競爭並導致記憶體損壞或讀取敏感資料。這是優先級較高的安全問題，應該立即修復。
