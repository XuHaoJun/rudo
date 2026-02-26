# [Bug]: WeakCrossThreadHandle Clone 未驗證執行緒親和性 - 與 resolve() 不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在多執行緒環境中使用 WeakCrossThreadHandle 可能會克隆到錯誤的執行緒 |
| **Severity (嚴重程度)** | Medium | 導致 API 使用不一致，但不會直接導致記憶體錯誤 |
| **Reproducibility (復現難度)** | Low | 易於重現 - 只需在錯誤的執行緒上克隆 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::clone()`, `handles/cross_thread.rs:525-533`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::clone()` 應該驗證呼叫執行緒是否為 origin_thread，與 `resolve()` 的行為一致。如果從非 origin 執行緒調用 clone，應該 panic 或返回錯誤。

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::clone()` 實現直接克隆內部資料結構，沒有任何執行緒檢查：

```rust
impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    fn clone(&self) -> Self {
        Self {
            weak: self.weak.clone(),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        }
    }
}
```

對比 `resolve()` 的行為（`handles/cross_thread.rs:153-160`）：
```rust
assert_eq!(
    std::thread::current().id(),
    self.origin_thread,
    "GcHandle::resolve() must be called on the origin thread (origin={:?}, current={:?}). \
     If the origin thread has terminated, use try_resolve() instead to get None.",
    self.origin_thread,
    std::thread::current().id(),
);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`WeakCrossThreadHandle::clone()` 與 `resolve()` API 設計不一致：

1. `resolve()` 要求必須在 origin 執行緒調用，否則 panic
2. `clone()` 可以在任何執行緒調用，不進行檢查
3. 這導致不一致的行為 - 使用者可能在不知情的情況下克隆了一個無法使用的 handle

雖然 clone 本身不會造成記憶體錯誤（只是複製指標），但這違反了 API 的一致性原則，使用者可能會在非 origin 執行緒上獲得一個看起來有效但無法 resolve 的 handle。

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
    let weak = gc.weak_cross_thread_handle();
    
    // 在執行緒 B 克隆 handle
    let other_thread = thread::spawn(move || {
        // 這應該 panic 或返回錯誤，但目前不會！
        let cloned = weak.clone();  // OK - 沒有檢查
        // cloned.resolve() 會 panic
    });
    
    other_thread.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `WeakCrossThreadHandle::clone()` 中添加執行緒檢查：

```rust
impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    fn clone(&self) -> Self {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "WeakCrossThreadHandle::clone() must be called on the origin thread. \
             Clone from a different thread is not allowed.",
        );
        Self {
            weak: self.weak.clone(),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 API 設計不一致的問題。Handle 應該在整個生命週期中保持執行緒親和性，clone 也不例外。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 soundness 問題，但違反了 API 一致性原則。可能導致困惑的行為。

**Geohot (Exploit 攻擊觀點):**
目前不可利用 - 克隆行為是安全的，只是 API 不一致。

---

## 備註

類似問題可能存在於 `GcHandle::clone()` - 需要驗證該實作是否已有執行緒檢查。

---

## Resolution (2026-02-27)

**Outcome:** Fixed.

Added `assert_eq!(current_thread, origin_thread)` to `WeakCrossThreadHandle::clone()` and `#[track_caller]` for panic location. `clone()` now requires origin thread, consistent with `resolve()` and `try_upgrade()`.

Updated `test_weak_clone_across_threads` to clone on origin thread before sending across threads. Added `test_weak_clone_wrong_thread_panics` to verify clone panics from non-origin thread.
