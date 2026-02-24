# [Bug]: Gc::cross_thread_handle() 缺少 is_under_construction 檢查 - Bug92 修復不完整

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 在物件構造過程中呼叫 cross_thread_handle 相對少見 |
| **Severity (嚴重程度)** | Medium | 可能導致存取未初始化資料，但需要特定時序才能觸發 |
| **Reproducibility (復現難度)** | High | 需要在物件構造過程中精確時機呼叫 cross_thread_handle，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::cross_thread_handle` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Gc::cross_thread_handle()` 應該檢查 `is_under_construction()` 標誌，與其他類似方法（如 `Gc::clone()`, `Gc::weak_cross_thread_handle()`）的行為一致。

### 實際行為 (Actual Behavior)

Bug92 修復了 `Gc::weak_cross_thread_handle()` 缺少 `is_under_construction()` 檢查的問題，但修復不完整：
- Bug92 修復了：`Gc::downgrade()`、`GcHandle::downgrade()` 和 `Gc::weak_cross_thread_handle()`
- Bug92 未修復：`Gc::cross_thread_handle()`

這導致 `Gc::cross_thread_handle()` 與其他 API 行為不一致。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:1285-1288`

```rust
pub fn cross_thread_handle(&self) -> crate::handles::GcHandle<T> {
    // ...
    unsafe {
        assert!(
            !(*ptr.as_ptr()).has_dead_flag() && (*ptr.as_ptr()).dropping_state() == 0,
            "Gc::cross_thread_handle: cannot create handle for dead or dropping Gc"
        );
        (*ptr.as_ptr()).inc_ref();
    }
    // ...
}
```

**對比**：正確的實現應該檢查：
```rust
assert!(
    !(*ptr.as_ptr()).is_under_construction()
        && !(*ptr.as_ptr()).has_dead_flag()
        && (*ptr.as_ptr()).dropping_state() == 0,
    "Gc::cross_thread_handle: cannot create handle for dead, dropping, or under construction Gc"
);
```

與 `Gc::clone()` (ptr.rs:1373-1378) 的對比：
```rust
// Gc::clone() 正確地檢查所有三個標誌
assert!(
    !(*gc_box_ptr).has_dead_flag()
        && (*gc_box_ptr).dropping_state() == 0
        && !(*gc_box_ptr).is_under_construction(),
    "Gc::clone: cannot clone a dead, dropping, or under construction Gc"
);
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Test {
    value: i32,
}

fn test_cross_thread_handle DURING construction() {
    // 嘗試在物件構造過程中建立 cross_thread_handle
    // 這應該 panic，但目前可能成功建立
    let gc = Gc::new_cyclic_weak(|weak_self| {
        // 在 closure 內部，物件仍在 construction 中
        let _handle = weak_self; // 這是一個 Weak，嘗試升級
        Test { value: 42 }
    });
    
    // 正確的做法應該在 Gc::new_cyclic_weak 完成後才能建立 cross_thread_handle
    let _handle = gc.cross_thread_handle();
}
```

Note: 真正的 bug 需要在物件構造過程中（GcBox::set_under_construction 為 true）呼叫 cross_thread_handle，這在正常使用中很難觸發。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Gc::cross_thread_handle` 添加 `is_under_construction()` 檢查：

```rust
pub fn cross_thread_handle(&self) -> crate::handles::GcHandle<T> {
    use std::sync::Arc;

    use crate::handles::GcHandle;

    let tcb = crate::heap::current_thread_control_block()
        .expect("cross_thread_handle called outside of GC context");

    let mut roots = tcb.cross_thread_roots.lock().unwrap();
    let handle_id = roots.allocate_id();

    let ptr = self.as_non_null();

    unsafe {
        assert!(
            !(*ptr.as_ptr()).is_under_construction()
                && !(*ptr.as_ptr()).has_dead_flag()
                && (*ptr.as_ptr()).dropping_state() == 0,
            "Gc::cross_thread_handle: cannot create handle for dead, dropping, or under construction Gc"
        );
        (*ptr.as_ptr()).inc_ref();
    }

    roots.strong.insert(handle_id, ptr.cast::<GcBox<()>>());

    drop(roots);

    GcHandle {
        ptr,
        origin_tcb: Arc::downgrade(&tcb),
        origin_thread: std::thread::current().id(),
        handle_id,
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間的 cross_thread_handle 會嘗試對可能尚未完全初始化的資料建立跨執行緒引用
- 這類似於 generational GC 中需要特別處理的「不成熟物件」
- 建議：cross_thread_handle 應該複製「完整的物件狀態」，包括 construction flag

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致存取未初始化的資料
- 這是一個 soundness 問題，儘管觸發條件較嚴格
- 與其他 API（如 clone, weak_cross_thread_handle）行為不一致，違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件被 cross_thread_handle 後可能導致 use-after-free
- 攻擊者可能透過精心設計的時序來利用這個 race condition
- 雖然難以穩定利用，但仍是潛在的攻击面
