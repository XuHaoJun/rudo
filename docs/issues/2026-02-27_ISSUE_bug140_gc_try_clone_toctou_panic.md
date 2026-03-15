# [Bug]: Gc::try_clone 存在 TOCTOU 導致可能 panic 而非返回 None

**Status:** Fixed
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確時序條件（try_clone 檢查後、clone 執行前狀態改變） |
| **Severity (嚴重程度)** | Medium | 導致 panic 而非返回 None，破壞 API 契約 |
| **Reproducibility (復現難度)** | High | 需極精確的執行時序才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::try_clone()`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`try_clone` 應該是 `try_*` 系列的非 panicking 版本：
- 當物件處於 dead、dropping 或 under construction 狀態時，應返回 `None`
- 不應 panic

### 實際行為 (Actual Behavior)

`try_clone` 檢查狀態後調用 `gc.clone()`，但 `clone()` 內部有自己的 assert 檢查。如果狀態在 `try_clone` 的檢查和 `clone()` 的 assert 之間改變，會導致 panic。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `ptr.rs:1187-1202`:

```rust
pub fn try_clone(gc: &Self) -> Option<Self> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        // 第一次檢查
        if (*gc_box_ptr).has_dead_flag()
            || (*gc_box_ptr).dropping_state() != 0
            || (*gc_box_ptr).is_under_construction()
        {
            return None;
        }
    }
    // BUG: 調用 clone() 會再次檢查並 assert
    Some(gc.clone())  // <-- TOCTOU: 狀態可能在此時改變
}
```

`clone()` 實現 (`ptr.rs:1476-1503`):
```rust
impl<T: Trace> Clone for Gc<T> {
    fn clone(&self) -> Self {
        // ...
        unsafe {
            // 第二次檢查 + assert
            assert!(
                !(*gc_box_ptr).has_dead_flag()
                    && (*gc_box_ptr).dropping_state() == 0
                    && !(*gc_box_ptr).is_under_construction(),
                "Gc::clone: cannot clone a dead, dropping, or under construction Gc"
            );
            (*gc_box_ptr).inc_ref();
        }
        // ...
    }
}
```

這是一個经典的 TOCTOU (Time-of-Check-Time-of-Use) 漏洞：
1. `try_clone` 檢查狀態 → 返回 None 或繼續
2. 另一個線程在此時開始 drop 物件
3. `clone()` 的 assert 檢查失敗 → panic

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let ready = Arc::new(AtomicBool::new(false));
    let ready_clone = ready.clone();
    
    let gc = Gc::new(Data { value: 42 });
    
    // 在另一個線程中 drop gc
    thread::spawn(move || {
        ready_clone.store(true, Ordering::SeqCst);
        drop(gc);
    });
    
    // 等待線程開始 drop
    while !ready.load(Ordering::SeqCst) {
        thread::yield();
    }
    
    // 嘗試 try_clone - 可能 panic 而非返回 None
    let result = Gc::try_clone(&gc);
    // 預期: result == None
    // 實際: 可能 panic
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩種修復方案：

### 方案 1: 在 try_clone 內直接實現 ref count 增加（推薦）

```rust
pub fn try_clone(gc: &Self) -> Option<Self> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        if (*gc_box_ptr).has_dead_flag()
            || (*gc_box_ptr).dropping_state() != 0
            || (*gc_box_ptr).is_under_construction()
        {
            return None;
        }
        
        // 直接增加 ref count，不調用 clone()
        if !(*gc_box_ptr).try_inc_ref_if_nonzero() {
            return None;
        }
        
        crate::gc::notify_created_gc();
        return Some(Gc {
            ptr: AtomicNullable::new(NonNull::new_unchecked(gc_box_ptr)),
            _marker: PhantomData,
        });
    }
}
```

### 方案 2: 移除 clone() 中的 assert，改為返回 None

這會改變 `clone()` 的行為，可能影響其他代碼。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個 API 一致性問題。`try_*` 函數應該是 non-panicking 的版本，這是 Rust 標準庫的慣例（例如 `Vec::try_get`）。TOCTOU 問題在並發環境下可能導致不穩定。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 UB（因為最終會 panic 而不是產生未定義行為），但這是 API 設計缺陷。`try_*` 函數的調用者期望得到 `Option`，而不是 panic。

**Geohot (Exploit 攻擊觀點):**
雖然目前主要影響是 availability（可用性），但在極端情況下攻擊者可能利用這個 panic 來進行 DoS 攻擊。

---

## ✅ Resolution Note (2026-03-01)

- Updated `Gc::try_clone` in `crates/rudo-gc/src/ptr.rs` to avoid calling `clone()` after a separate pre-check.
- `try_clone` now performs an atomic `try_inc_ref_if_nonzero()` and then runs a post-increment state check (`dead` / `dropping` / `under construction`).
- If state changes after increment, it rolls back via `GcBox::dec_ref(...)` and returns `None` instead of panicking.
- Targeted verification run: `cargo test -p rudo-gc --test basic test_try_clone -- --test-threads=1` passed.
- The exact race window remains hard to deterministically reproduce with a stable test in current harness, so tag remains `Not Verified`.
