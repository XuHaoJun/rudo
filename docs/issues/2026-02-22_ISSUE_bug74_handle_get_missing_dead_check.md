# [Bug]: Handle::get() / AsyncHandle::get() 缺少 dead_flag / dropping_state 檢查導致潛在 UAF

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 scope 存活但物件已死亡時存取 handle |
| **Severity (嚴重程度)** | High | 可能導致 Use-After-Free，存取已釋放記憶體 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::get()` in `handles/mod.rs:300-307`, `AsyncHandle::get()` in `handles/async.rs:563-581`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當呼叫 `Handle::get()` 或 `AsyncHandle::get()` 時，如果物件已經死亡（`has_dead_flag()` 為 true）或正在被 drop（`dropping_state() != 0`），應該返回 `None` 或 panic。

這與 `AsyncGcHandle::downcast_ref()` 的行為一致，後者有正確的檢查。

### 實際行為 (Actual Behavior)

目前 `Handle::get()` 和 `AsyncHandle::get()` **沒有**檢查：
- `has_dead_flag()`
- `dropping_state()`

直接返回值的引用而不檢查物件狀態，導致可能存取已死亡或正在 dropping 的物件。

### 影響範圍

此問題影響以下方法：
- `Handle::get()` (handles/mod.rs:300-307)
- `AsyncHandle::get()` (handles/async.rs:563-581)

對比 `AsyncGcHandle::downcast_ref()` 有正確的檢查：
```rust
// handles/async.rs:1214-1218
let gc_box = &*gc_box_ptr;
if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 {
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：**
1. `handles/mod.rs:300-307` (`Handle::get()`)
2. `handles/async.rs:563-581` (`AsyncHandle::get()`)

這兩個方法直接返回 `gc_box.value()` 而不檢查物件的存活狀態。雖然 handle scope 存活保證 handle slot 有效，但物件本身可能已經死亡（所有強引用已 drop，dead_flag 已設置）。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::HandleScope;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = HandleScope::new(&tcb);
    
    // 創建物件並取得 handle
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    // drop 物件（不是 handle！）
    drop(gc);
    
    // 嘗試手動觸發 GC 來設置 dead_flag
    // 此時 handle 仍然有效（scope 還沒 drop）
    // 但底層物件可能已經死亡
    
    // 這裡調用 get() 可能會存取已釋放的記憶體
    // let _ = handle.get(); // UAF!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Handle::get()` 和 `AsyncHandle::get()` 中新增檢查：

```rust
pub fn get(&self) -> Option<&T> {
    unsafe {
        let slot = &*self.slot;
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        let gc_box = &*gc_box_ptr;
        
        // 新增檢查
        if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 {
            return None;
        }
        
        Some(gc_box.value())
    }
}
```

或者參考 `AsyncGcHandle::downcast_ref()` 的模式，返回 `Option<&T>` 而非 `&T`。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Handle scope 存活的意義是 handle slot 有效，但並不保證底層物件仍然活著。當所有強引用都被 drop 時，物件會被標記為 dead，即使 scope 還沒結束。這種情況下存取值會導致 UAF。

**Rustacean (Soundness 觀點):**
直接返回引用而不檢查狀態违反了記憶體安全。在物件已死亡的情況下返回引用是未定義行為。

**Geohot (Exploit 觀點):**
這是一個經典的 Use-After-Free 模式。攻擊者可能利用這個漏洞在物件被回收後仍然存取其記憶體。
