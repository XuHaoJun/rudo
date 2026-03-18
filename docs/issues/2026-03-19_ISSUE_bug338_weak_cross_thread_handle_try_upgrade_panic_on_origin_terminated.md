# [Bug]: WeakCrossThreadHandle::try_upgrade panics when origin thread terminates

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 origin thread 終止後呼叫 try_upgrade |
| **Severity (嚴重程度)** | Medium | 導致 panic，阻斷程式執行 |
| **Reproducibility (復現難度)** | Low | 需模擬 origin thread 終止場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::try_upgrade()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`WeakCrossThreadHandle::try_upgrade()` 在 origin thread 終止時會 panic，但這與以下 API 行為不一致：

1. `Weak::try_upgrade()` - 失敗時返回 `None`，從不 panic
2. `WeakCrossThreadHandle::try_resolve()` - origin thread 終止時返回 `None`

### 預期行為 (Expected Behavior)

`try_upgrade()` 應該在 origin thread 終止時返回 `None`，就像 `try_resolve()` 一樣。

### 實際行為 (Actual Behavior)

`try_upgrade()` 在 origin thread 終止時 panic，訊息為：
```
WeakCrossThreadHandle::try_upgrade: origin thread has terminated (origin={...}). 
Use try_resolve() instead.
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs:722-730`：

```rust
pub fn try_upgrade(&self) -> Option<Gc<T>> {
    // Check TCB liveness BEFORE the ThreadId comparison...
    if self.origin_tcb.upgrade().is_none() {
        panic!(
            "WeakCrossThreadHandle::try_upgrade: origin thread has terminated (origin={:?}). \
             Use try_resolve() instead.",
            self.origin_thread
        );
    }
    // ...
}
```

對比 `try_resolve()` 在 `handles/cross_thread.rs:679-687`：

```rust
pub fn try_resolve(&self) -> Option<Gc<T>> {
    self.origin_tcb.upgrade()?;  // 返回 None 如果 origin 終止
    if std::thread::current().id() != self.origin_thread {
        return None;
    }
    self.weak.upgrade()
}
```

`try_upgrade()` 應該使用 `?` operator 而非 `panic!()`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let handle = thread::spawn(|| {
        let gc: Gc<Data> = Gc::new(Data { value: 42 });
        let weak = gc.weak_cross_thread_handle();
        weak  // 返回 weak handle 到主線程
    }).join().unwrap();

    // Origin thread 已終止
    // 預期: try_upgrade() 返回 None
    // 實際: panic!
    let result = handle.try_upgrade();
    println!("Result: {:?}", result);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `try_upgrade()` 中的 panic 改為返回 `None`：

```rust
pub fn try_upgrade(&self) -> Option<Gc<T>> {
    // 返回 None 如果 origin thread 終止（與 try_resolve 一致）
    self.origin_tcb.upgrade()?;
    if std::thread::current().id() != self.origin_thread {
        return None;
    }
    self.weak.try_upgrade()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從 GC 角度來看，weak handle 在 origin thread 終止後應該仍然可以嘗試升級（透過 orphan table）。返回 `None` 是合理的行為。

**Rustacean (Soundness 觀點):**
`try_*` 函數應該返回 `Option` 而非 panic。這是 API 一致性問題。

**Geohot (Exploit 觀點):**
此 bug 不涉及安全問題，只是 API 使用體驗問題。
