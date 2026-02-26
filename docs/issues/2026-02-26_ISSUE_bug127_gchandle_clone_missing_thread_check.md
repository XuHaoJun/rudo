# [Bug]: GcHandle::clone 未驗證執行緒親和性 - 與 resolve() 不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在多執行緒環境中使用 GcHandle 可能會克隆到錯誤的執行緒 |
| **Severity (嚴重程度)** | Medium | 導致 API 使用不一致，但不會直接導致記憶體錯誤 |
| **Reproducibility (復現難度)** | Low | 易於重現 - 只需在錯誤的執行緒上克隆 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::clone()`, `handles/cross_thread.rs:320-355`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::clone()` 應該驗證呼叫執行緒是否為 origin_thread，與 `resolve()` 的行為一致。如果從非 origin 執行緒調用 clone，應該 panic 或返回錯誤。

### 實際行為 (Actual Behavior)

`GcHandle::clone()` 實現直接克隆內部資料結構，沒有任何執行緒檢查：

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        if self.handle_id == HandleId::INVALID {
            panic!("cannot clone an unregistered GcHandle");
        }
        // 缺少：執行緒親和性檢查！
        let (new_id, origin_tcb) = self.origin_tcb.upgrade().map_or_else(...)
```

對比 `resolve()` 的行為（`handles/cross_thread.rs:148-160`）：
```rust
pub fn resolve(&self) -> Gc<T> {
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::resolve: handle has been unregistered"
    );
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        "GcHandle::resolve() must be called on the origin thread..."
    );
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`GcHandle::clone()` 與 `resolve()` API 設計不一致：

1. `resolve()` 要求必須在 origin 執行緒調用，否則 panic
2. `clone()` 可以在任何執行緒調用，不進行檢查
3. 這導致不一致的行為 - 使用者可能在不知情的情況下克隆了一個無法使用的 handle

此問題與 Bug 124（WeakCrossThreadHandle::clone 缺少執行緒檢查）為同一系列問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 在執行緒 A 建立 Gc
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // 在執行緒 B 克隆 handle
    let other_thread = thread::spawn(move || {
        // 這應該 panic 或返回錯誤，但目前不會！
        let cloned = handle.clone();  // OK - 沒有檢查
        // cloned.resolve() 會 panic
    });
    
    other_thread.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::clone()` 中添加執行緒檢查：

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "GcHandle::clone() must be called on the origin thread. \
             Clone from a different thread is not allowed.",
        );
        
        if self.handle_id == HandleId::INVALID {
            panic!("cannot clone an unregistered GcHandle");
        }
        // ... rest of the implementation
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 API 設計不一致的問題。Handle 應該在整個生命週期中保持執行緒親和性，clone 也不例外。與 Bug 124 的 WeakCrossThreadHandle 問題相同。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 soundness 問題，但違反了 API 一致性原則。可能導致困惑的行為。

**Geohot (Exploit 攻擊觀點):**
目前不可利用 - 克隆行為是安全的，只是 API 不一致。

---

## 備註

此問題與 Bug 124（WeakCrossThreadHandle::clone 缺少執行緒檢查）為同一系列問題。應該統一修復這兩個問題，確保 cross-thread handle API 的一致性。

---

## Resolution (2026-02-27)

**Outcome:** Fixed.

Added origin-thread check to `GcHandle::clone()` when origin is still alive. When origin has terminated (orphaned handle), clone is allowed from any thread to preserve bug4 test behavior (handles received from `join()`). Uses `origin_tcb.upgrade().is_some()` to distinguish: if TCB exists, require origin thread; if TCB is None (orphaned), allow clone from any thread.

Added `#[track_caller]` for panic location. Added `test_gchandle_clone_wrong_thread_panics` to verify clone panics from non-origin thread.
