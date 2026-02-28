# [Bug]: WeakCrossThreadHandle::clone 不允許跨執行緒複製已終止的 origin handle

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 origin 執行緒終止後從另一個執行緒複製 WeakCrossThreadHandle |
| **Severity (嚴重程度)** | Medium | 導致 panic，造成程式當機 |
| **Reproducibility (復現難度)** | Low | 容易重現 - 只需建立 handle，終止執行緒，然後嘗試複製 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`WeakCrossThreadHandle::clone()` 應該允許在 origin 執行緒已終止（變成 orphan handle）時，從任何執行緒複製。這與 `GcHandle::clone()` 的行為一致。

### 實際行為 (Actual Behavior)
`WeakCrossThreadHandle::clone()` 總是要求 current thread == origin_thread，即使 origin 執行緒已經終止也會 panic。這與 `GcHandle::clone()` 的行為不一致。

**程式碼位置:** `crates/rudo-gc/src/handles/cross_thread.rs:555-561`

```rust
impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    fn clone(&self) -> Self {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "WeakCrossThreadHandle::clone() must be called on the origin thread. \
             Clone from a different thread is not allowed."
        );
        // ...
    }
}
```

相比之下，`GcHandle::clone()` 正確地處理了這種情況：

```rust
// crates/rudo-gc/src/handles/cross_thread.rs:345-352
if self.origin_tcb.upgrade().is_some() {
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        "GcHandle::clone() must be called on the origin thread. \
         Clone from a different thread is not allowed."
    );
}
// 當 origin_tcb.upgrade() 為 None（執行緒已終止），允許從任何執行緒複製
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`WeakCrossThreadHandle::clone()` 實作缺少對 orphan handle 的處理。當 origin 執行緒終止後：
1. `GcHandle` 可以從任何執行緒克隆（因為有明確檢查 `origin_tcb.upgrade().is_some()`）
2. `WeakCrossThreadHandle` 仍然強制要求 origin 執行緒，導致 panic

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // 在主執行緒建立 Gc 並取得 handle
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // 降級為 WeakCrossThreadHandle
    let weak_handle = handle.downgrade();
    
    // 在另一個執行緒中嘗試複製 - 這會 panic!
    let handle2 = thread::spawn(move || {
        let cloned = weak_handle.clone(); // PANIC!
        cloned
    }).join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `WeakCrossThreadHandle::clone()` 以允許在 origin 執行緒終止後從任何執行緒複製：

```rust
impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    fn clone(&self) -> Self {
        // 與 GcHandle::clone() 一致的行為：
        // 僅在 origin 執行緒仍然存活時要求 origin_thread
        if self.origin_tcb.upgrade().is_some() {
            assert_eq!(
                std::thread::current().id(),
                self.origin_thread,
                "WeakCrossThreadHandle::clone() must be called on the origin thread. \
                 Clone from a different thread is not allowed."
            );
        }
        // 當 origin 執行緒已終止（orphan），允許從任何執行緒複製
        Self {
            weak: self.weak.clone(),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        }
    }
}
```

同樣的修復也應該應用於其他 `WeakCrossThreadHandle` 方法，例如 `resolve()`、`try_resolve()` 等，確保它們在 origin 執行緒終止時的行為與 `GcHandle` 一致。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
弱引用在跨執行緒情境下應該與強引用有一致的行為。當 origin 執行緒終止後，handle 會變成 orphan，這時候應該允許從任何執行緒操作，因為此時已經沒有執行緒本地的概念。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，而是 API 一致性問題。當前實作會導致 panic，而非未定義行為。但這仍然是一個重要的 bug，因為它違反了最少驚訝原則。

**Geohot (Exploit 觀點):**
這不是安全漏洞，但會造成可用性問題。攻擊者無法利用這個問題，但使用者可能會因為这个不一致的行為而感到困惑。
