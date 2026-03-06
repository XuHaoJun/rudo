# [Bug]: GcHandle::downgrade 缺少執行緒親和性檢查 - 與 clone() 和 resolve() 不一致

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在多執行緒環境中使用 GcHandle::downgrade 可能從錯誤的執行緒調用 |
| **Severity (嚴重程度)** | Medium | 導致 API 使用不一致，但不會直接導致記憶體錯誤 |
| **Reproducibility (復現難度)** | Low | 易於重現 - 只需在錯誤的執行緒上調用 downgrade |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade()`, `handles/cross_thread.rs:290-335`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::downgrade()` 應該驗證呼叫執行緒是否為 origin_thread，與 `clone()` 和 `resolve()` 的行為一致。如果從非 origin 執行緒調用，應該 panic 或返回錯誤。

### 實際行為 (Actual Behavior)

`GcHandle::downgrade()` 實現直接創建 WeakCrossThreadHandle，沒有任何執行緒檢查：

```rust
impl<T: Trace + 'static> GcHandle<T> {
    pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
        assert!(
            self.handle_id != HandleId::INVALID,
            "GcHandle::downgrade: cannot downgrade an unregistered GcHandle"
        );
        // Hold lock during check-and-inc_weak to prevent TOCTOU with unregister/drop.
        // Same pattern as GcHandle::clone() and GcHandle::resolve().
        if let Some(tcb) = self.origin_tcb.upgrade() {
            // ... 沒有執行緒檢查！
        }
        // ...
    }
}
```

對比 `GcHandle::clone()` 的行為（`handles/cross_thread.rs:363-368`）：
```rust
assert_eq!(
    std::thread::current().id(),
    self.origin_thread,
    "GcHandle::clone() must be called on the origin thread. \
     Clone from a different thread is not allowed."
);
```

對比 `GcHandle::resolve()` 的行為（`handles/cross_thread.rs:181-188`）：
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

`GcHandle::downgrade()` 與 `clone()` 和 `resolve()` API 設計不一致：

1. `resolve()` 要求必須在 origin 執行緒調用，否則 panic
2. `clone()` 要求必須在 origin 執行緒調用（除非 origin 已終止，成為 orphan），否則 panic
3. `downgrade()` 可以在任何執行緒調用，不進行檢查
4. 這導致不一致的行為 - 使用者可能在不知情的情況下從錯誤執行緒調用 downgrade

雖然 downgrade 本身不會造成記憶體錯誤（只是創建一個 Weak handle），但這違反了 API 的一致性原則。`downgrade()` 內部訪問 gc_box 的狀態（`has_dead_flag()`, `dropping_state()`, `is_under_construction()`）並調用 `inc_weak()`，這些操作應該在 origin 執行緒上執行以保持一致性。

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
    
    // 在執行緒 B 調用 downgrade
    let other_thread = thread::spawn(move || {
        // 這應該 panic 或返回錯誤，但目前不會！
        let weak = handle.downgrade();  // OK - 沒有檢查
        // weak.resolve() 會 panic（因為在錯誤執行緒）
    });
    
    other_thread.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::downgrade()` 中添加執行緒檢查，與 `clone()` 的模式一致：

```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::downgrade: cannot downgrade an unregistered GcHandle"
    );
    
    // Require origin thread when origin is still alive (consistent with clone() and resolve()).
    // When origin has terminated (orphaned handle), downgrade is allowed from any thread.
    let origin_tcb = self.origin_tcb.upgrade();
    if origin_tcb.is_some() {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "GcHandle::downgrade() must be called on the origin thread. \
             Downgrade from a different thread is not allowed."
        );
    }
    
    // ... 其餘現有代碼
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 API 設計不一致的問題。Handle 應該在整個生命週期中保持執行緒親和性，downgrade 也不例外。`downgrade()` 內部訪問 gc_box 狀態並調用 `inc_weak()`，這些操作在 origin 執行緒上執行更安全。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 soundness 問題，但違反了 API 一致性原則。可能導致困惑的行為。

**Geohot (Exploit 攻擊觀點):**
目前不可利用 - downgrade 行為是安全的，只是 API 不一致。

---

## 備註

類似的問題可能存在於其他方法 - 需要全面審查 cross_thread.rs 中的方法是否有不一致的執行緒親和性要求。
