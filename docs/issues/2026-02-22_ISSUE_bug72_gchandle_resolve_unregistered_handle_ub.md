# [Bug]: GcHandle::resolve() / try_resolve() 未檢查 handle_id 是否已失效

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能在呼叫 unregister() 後忘記丟棄 handle，繼續使用導致問題 |
| **Severity (嚴重程度)** | High | 可能導致 use-after-free，讀取已釋放或已重複使用的記憶體 |
| **Reproducibility (復現難度)** | Low | 容易重現：建立 handle → unregister() → resolve() |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`, `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`resolve()` 和 `try_resolve()` 應該在解引用指標前驗證 handle 是否仍然有效（即 `handle_id != HandleId::INVALID`）。

這與以下方法的行為一致：
- `GcHandle::clone()` - 在 line 260 檢查 `handle_id == HandleId::INVALID`
- `GcHandle::unregister()` - 在 line 105 返回 early 如果已失效
- `GcHandle::Drop` - 在 line 311 返回 early 如果已失效

### 實際行為 (Actual Behavior)

`resolve()` (line 147-175) 和 `try_resolve()` (line 203-218) **沒有**檢查 `handle_id == HandleId::INVALID`。

直接解引用 `self.ptr.as_ptr()` 並進行檢查：
- `is_under_construction()`
- `has_dead_flag()`
- `dropping_state()`

如果記憶體已被釋放或重用，這些檢查會讀取無效記憶體（undefined behavior）。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/cross_thread.rs:147-175` (`resolve()`) 和 `handles/cross_thread.rs:203-218` (`try_resolve()`)

對比 `GcHandle::clone()` (line 258-302) 有正確的檢查：

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        if self.handle_id == HandleId::INVALID {
            panic!("cannot clone an unregistered GcHandle");
        }
        // ...
    }
}
```

但 `resolve()` 和 `try_resolve()` 缺少此檢查：

```rust
pub fn resolve(&self) -> Gc<T> {
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        // ...
    );
    // 缺少 handle_id 有效性檢查！！！
    unsafe {
        let gc_box = &*self.ptr.as_ptr();  // 如果記憶體已釋放，這是 UB
        // ...
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let mut handle = gc.cross_thread_handle();
    
    // Step 1: Unregister the handle (simulates Drop behavior)
    handle.unregister();
    
    // Step 2: Try to resolve after unregister
    // This should panic or return None, but instead causes UB
    let resolved = handle.resolve();  // UB: 解引用已失效的指標
    
    println!("{}", resolved.value);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `resolve()` 和 `try_resolve()` 開頭添加 `handle_id` 有效性檢查：

```rust
pub fn resolve(&self) -> Gc<T> {
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::resolve: handle has been unregistered"
    );
    // ... existing code
}

pub fn try_resolve(&self) -> Option<Gc<T>> {
    if self.handle_id == HandleId::INVALID {
        return None;
    }
    // ... existing code
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
當 handle 被 unregister() 後，root entry 被移除，物件變得可以被 GC 回收。如果記憶體被釋放並重用，resolve() 會讀取新物件的 GcBox header，導致錯誤的 ref count 操作。

**Rustacean (Soundness 觀點):**
這是經典的 use-after-free / dangling pointer 漏洞。雖然看起來是「開發者錯誤」（不應該在 unregister 後使用），但 API 應該防止這種錯誤使用，避免 undefined behavior。

**Geohot (Exploit 觀點):**
攻擊者可以透過精心設計的時序：
1. 讓 victim 建立 GcHandle
2. 觸發 unregister()（或 handle 被 drop）
3. 快速呼叫 resolve() 
4. 如果記憶體已被重用，可能讀取到攻擊者控制的資料

---

## Resolution (2026-02-26)

**Outcome:** Already fixed.

The current `GcHandle::resolve()` (lines 148-151) asserts `self.handle_id != HandleId::INVALID` before dereferencing. `GcHandle::try_resolve()` (lines 208-210) returns `None` when `handle_id == HandleId::INVALID`. Both methods validate handle validity before accessing the pointer.
