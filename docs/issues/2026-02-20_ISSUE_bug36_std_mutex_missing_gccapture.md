# [Bug]: std::sync::Mutex 缺少 GcCapture 實作導致指標遺漏

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會在 std::sync::Mutex 中存儲 GC 指標 |
| **Severity (嚴重程度)** | High | 導致 GC 無法掃描到 Mutex 內部的指標，可能導致記憶體錯誤 |
| **Reproducibility (復現難度)** | Medium | 需要在 GC 掃描時 Mutex 未被鎖定 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` impl for `std::sync::Mutex<T>`, `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`std::sync::Mutex<T>` 應該實作 `GcCapture` trait，使得 GC 可以掃描 Mutex 內部的 GC 指標。這與 `std::sync::RwLock<T>` 的行為一致（cell.rs:567-579）。

### 實際行為 (Actual Behavior)
`std::sync::Mutex<T>` 沒有實作 `GcCapture` trait。當 GC 嘗試掃描根集時，無法捕捉到存在於 `std::sync::Mutex<T>` 內部的 GC 指標，導致這些指標被錯誤地視為垃圾。

**與現有 bug 的區別：**
- bug33: `GcMutex`（rudo-gc 自己的 Mutex）缺少 GcCapture - **已記錄**
- bug35: `std::sync::RwLock` 有 GcCapture 但使用 `try_read()` - **已記錄**
- **本 bug**: `std::sync::Mutex` 完全缺少 GcCapture - **新發現**

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/cell.rs`

`std::sync::RwLock<T>` 已有 GcCapture 實作（lines 567-579）：
```rust
impl<T: GcCapture + 'static> GcCapture for RwLock<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Ok(value) = self.try_read() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

但 `std::sync::Mutex<T>` 沒有對應的實作。搜尋結果顯示：
- `RwLock<T>` 有 GcCapture：cell.rs:567
- `Mutex<T>` 沒有 GcCapture：**未找到實作**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
    gc_ptr: Option<Gc<Data>>,
}

fn main() {
    let cell = GcCell::new(Mutex::new(Data {
        value: 0,
        gc_ptr: None,
    }));
    
    // 在 Mutex 內部存儲 GC 指標
    {
        let mut guard = cell.write().unwrap();
        guard.gc_ptr = Some(Gc::new(Data {
            value: 42,
            gc_ptr: None,
        }));
    }
    
    // 嘗試觸發 GC
    // 由於 std::sync::Mutex 缺少 GcCapture，
    // GC 無法掃描到 Mutex 內部的 Gc<Data>指標
    for _ in 0..10 {
        rudo_gc::collect_full();
        thread::sleep(Duration::from_millis(10));
    }
    
    // 訪問 Mutex 內部的 GC 指標
    // 如果指標被錯誤回收，這裡可能會出現 use-after-free
    let guard = cell.write().unwrap();
    if let Some(ref gc) = guard.gc_ptr {
        println!("Value: {}", gc.value); // 可能會崩潰！
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cell.rs` 中添加 `std::sync::Mutex<T>` 的 GcCapture 實作：

```rust
use std::sync::Mutex as StdMutex;

impl<T: GcCapture + 'static> GcCapture for StdMutex<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Ok(value) = self.lock() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

**注意**：這裡使用 `lock()`（會阻塞）而非 `try_lock()`（可能失敗），因為我們需要在 GC 掃描時確保能夠訪問內部資料。這與 bug35（使用 try_read()）的解決方案一致。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
所有可用於存儲 GC 指標的可變容器都應該實作 GcCapture，確保 GC 可以可靠地掃描所有根。std::sync::Mutex 與 std::sync::RwLock 應該有一致的行為。

**Rustacean (Soundness 觀點):**
缺少 GcCapture 會導致記憶體不安全。當 GC 無法掃描到 Mutex 內部的指標時，這些指標可能被錯誤地回收，導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以透過在 GC 掃描時持有 Mutex 鎖，阻止 GC 掃描到內部的指標，導致指標被錯誤回收。雖然利用難度較高，但這是一個潛在的記憶體損壞向量。

---

## 📌 與現有 Bug 的關係

- **bug33**: GcMutex 缺少 GcCapture - 相關但不同（GcMutex 是 rudo-gc 的類型）
- **bug35**: std::sync::RwLock 有 GcCapture 但使用 try_read() - 相關問題
- **本 bug**: std::sync::Mutex 完全缺少 GcCapture - 新發現

---

**Resolution:** Added `GcCapture` impl for `std::sync::Mutex<T>` in cell.rs, using blocking `lock()` to reliably capture inner GC pointers, consistent with `std::sync::RwLock` (bug35).
