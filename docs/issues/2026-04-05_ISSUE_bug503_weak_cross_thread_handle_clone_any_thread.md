# [Bug]: WeakCrossThreadHandle::clone 允許從任何執行緒克隆 - 與 GcHandle::clone API 不一致

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在多執行緒環境中使用 WeakCrossThreadHandle 可能會克隆到錯誤的執行緒 |
| **Severity (嚴重程度)** | Medium | 導致 API 使用不一致，但不會直接導致記憶體錯誤 |
| **Reproducibility (重現難度)** | Very Low | 易於重現 - 只需在錯誤的執行緒上克隆 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::clone()`, `handles/cross_thread.rs:1028-1041`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.19

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::clone()` 應該驗證呼叫執行緒是否為 origin_thread，與 `GcHandle::clone()` 的行為一致。如果從非 origin 執行緒調用 clone，當 TCB 仍然存活時應該 panic。

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::clone()` 實現明確允許從任何執行緒克隆（根據 line 1031 的註釋），與 `GcHandle::clone()` 不一致：

**`GcHandle::clone()` (lines 716-722):**
```rust
} else if let Some(tcb) = self.origin_tcb.upgrade() {
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        "GcHandle::clone() must be called on the origin thread. \
             Clone from a different thread is not allowed."
    );
```

**`WeakCrossThreadHandle::clone()` (lines 1028-1041):**
```rust
impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    #[track_caller]
    fn clone(&self) -> Self {
        // Clone is allowed from any thread. The weak ref does not register roots or expose T;
        // try_resolve/resolve enforce origin-thread affinity when actually accessing the value.
        // This matches GcHandle::clone behavior when origin has terminated (bug156), and avoids
        // a race where join() returns before TCB is dropped (upgrade still Some).
        Self {
            weak: self.weak.clone(),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        }
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`WeakCrossThreadHandle::clone()` 與 `GcHandle::clone()` API 設計不一致：

1. `GcHandle::clone()` 當 TCB 存活時要求 origin thread，否則 panic
2. `WeakCrossThreadHandle::clone()` 允許從任何執行緒克隆
3. 這導致不一致的行為 - 使用者可能在非 origin 執行緒上獲得一個看起來有效但無法 resolve 的 handle

雖然 `WeakCrossThreadHandle` 是 weak ref 本身不會造成 memory error，但這使得 API 使用混亂。

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
    
    // 在執行緒 B 克隆 handle - 目前不會 panic
    let other_thread = thread::spawn(move || {
        let cloned = weak.clone();  // OK - 沒有檢查
        // cloned.resolve() 會 panic
    });
    
    other_thread.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `WeakCrossThreadHandle::clone()` 中添加執行緒檢查（當 TCB 仍然存活時）：

```rust
impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    #[track_caller]
    fn clone(&self) -> Self {
        // When origin TCB is still alive, require origin thread for consistency
        // with GcHandle::clone(). When TCB is dead (orphaned), allow any thread
        // to clone to handle the race where join() returns before TCB is dropped.
        if let Some(tcb) = self.origin_tcb.upgrade() {
            assert_eq!(
                std::thread::current().id(),
                self.origin_thread,
                "WeakCrossThreadHandle::clone() must be called on the origin thread. \
                 Clone from a different thread is not allowed."
            );
        }
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
這是 API 設計不一致的問題。Handle 應該在整個生命週期中保持執行緒親和性，clone 也不例外。Weak ref 不註冊 root，所以實際上 clone 本身是安全的，但這造成 API 使用混亂。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 soundness 問題（weak ref 不影響 memory safety），但違反了 API 一致性原則。可能導致困惑的行為和使用者錯誤。

**Geohot (Exploit 攻擊觀點):**
目前不可利用 - 克隆行為是安全的，只是 API 不一致。攻擊者可能利用這點進行 social engineering 攻擊。

---

## 備註

- Bug124 記錄了相同問題但針對 `WeakCrossThreadHandle::clone`
- Bug127 記錄了相同問題但針對 `GcHandle::clone`
- 兩者都應該有一致的行為：當 origin TCB 存活時要求 origin thread
