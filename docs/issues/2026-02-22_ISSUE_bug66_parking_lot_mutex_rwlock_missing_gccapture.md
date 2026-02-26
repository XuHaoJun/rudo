# [Bug]: parking_lot::Mutex 與 parking_lot::RwLock 缺少 GcCapture 實作導致指標遺漏

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在類型中包含 Gc<T> 指針並使用停車場鎖 |
| **Severity (嚴重程度)** | High | 導致 GC 無法追蹤指標，可能造成記憶體洩露或 use-after-free |
| **Reproducibility (復現難度)** | Medium | PoC 相對簡單，但需確認 Gc<T> 在鎖內部 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** parking_lot::Mutex, parking_lot::RwLock
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`parking_lot::Mutex<T>` 與 `parking_lot::RwLock<T>` 缺少 `GcCapture` trait 實作。

當 `Gc<T>` 指針存於 `parking_lot::Mutex<T>` 或 `parking_lot::RwLock<T>` 內部時，GC 將無法正確追蹤這些指標，導致：
1. 指標可能被錯誤回收
2. 標記階段可能遺漏這些指標

### 預期行為
`GcCapture` 應該能夠從 `parking_lot::Mutex<T>` 與 `parking_lot::RwLock<T>` 內部提取 GC 指針。

### 實際行為
沒有 `GcCapture` 實作，GC 在追蹤時會遺漏這些類型內部的指標。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs` 中，`std::sync::Mutex<T>` 與 `std::sync::RwLock<T>` 已有 `GcCapture` 實作（bug35, bug36），但 `parking_lot::Mutex<T>` 與 `parking_lot::RwLock<T>` 卻沒有。

現有實作模式（cell.rs:596-624）：
```rust
impl<T: GcCapture + 'static> GcCapture for StdMutex<T> {
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Use blocking lock() to reliably capture all GC pointers
        if let Ok(guard) = self.lock() {
            guard.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

缺少 `parking_lot` 版本的實作。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let mutex = Arc::new(Mutex::new(Gc::new(Data { value: 42 })));
    
    // 模擬 GC 追蹤 - 這會失敗因為 GcCapture 未實作
    let ptrs = Vec::new();
    // mutex.capture_gc_ptrs_into(&mut ptrs); // 編譯錯誤！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cell.rs` 中新增：

```rust
impl<T: GcCapture + 'static> GcCapture for parking_lot::Mutex<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Use blocking lock() to reliably capture all GC pointers, same as StdMutex.
        let guard = self.lock();
        guard.capture_gc_ptrs_into(ptrs);
    }
}

impl<T: GcCapture + 'static> GcCapture for parking_lot::RwLock<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Use blocking read() to reliably capture all GC pointers, same as RwLock.
        let guard = self.read();
        guard.capture_gc_ptrs_into(ptrs);
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
停車場鎖是效能關鍵路徑上常用的同步原語。缺少 GcCapture 會導致 GC 無法正確追蹤指標，這與 std::sync 版本的問題相同（bug35, bug36）。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果 GC 無法追蹤指標，包含 Gc<T> 的 parking_lot 鎖可能導致 use-after-free 或記憶體洩露。

**Geohot (Exploit 觀點):**
攻擊者可能利用此漏洞，通過控制何時 GC 運行來觸發 use-after-free。

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

Added `GcCapture` implementations for `parking_lot::Mutex<T>` and `parking_lot::RwLock<T>` in `cell.rs`, following the same pattern as `std::sync::Mutex` and `std::sync::RwLock`. Both use blocking `lock()`/`read()` to reliably capture all GC pointers before delegating to the inner value's `capture_gc_ptrs_into()`.
