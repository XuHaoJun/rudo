# [Bug]: Gc::deref 未檢查 null 指標導致 Null Pointer Dereference

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 可能透過 Weak::upgrade 失敗或 Gc::from_raw(null) 觸發 |
| **Severity (嚴重程度)** | Critical | 導致 undefined behavior (null pointer dereference) |
| **Reproducibility (復現難度)** | Medium | 需要构造 null Gc 指针 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Gc::deref in ptr.rs:1459-1471
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`Gc<T>` 的 `Deref` 實現在解引用指標前未檢查指標是否為 null，與其他方法（如 `Clone` 和 `try_deref`）的行為不一致。

### 預期行為
`Gc::deref` 應該在指標為 null 時 panic 或返回錯誤，類似於 `try_deref` 的行為。

### 實際行為
`Gc::deref` 直接解引用指標（`*gc_box_ptr`），如果指標為 null會導致 undefined behavior。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs:1459-1471`：

```rust
impl<T: Trace> Deref for Gc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        let ptr = self.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        unsafe {
            // 缺少 null 檢查！
            assert!(
                !(*gc_dead_flag()_box_ptr).has  // <-- 如果 ptr 為 null，這裡會 UB
                    && (*gc_box_ptr).dropping_state() == 0
                    && !(*gc_box_ptr).is_under_construction(),
                "Gc::deref: cannot dereference a dead, dropping, or under construction Gc"
            );
            &(*gc_box_ptr).value
        }
    }
}
```

對比 `Clone` 實現（line 1477）正確檢查 null：
```rust
impl<T: Trace> Clone for Gc<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {  // <-- 正確的 null 檢查
            return Self { /* null */ };
        }
        // ...
    }
}
```

同樣，`try_deref()` 也正確檢查 null：
```rust
fn try_deref(&self) -> Option<&T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    if ptr.is_null() {  // <-- 正確的 null 檢查
        return None;
    }
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::ptr::null;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // 透過 from_raw 构造 null Gc
    let gc: Gc<Data> = unsafe { Gc::from_raw(null()) };
    
    // 嘗試解引用 - 會觸發 UB
    let _ = *gc; // 未定義行為！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `ptr.rs:1459-1471` 的 `deref` 函數開頭新增 null 檢查：

```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        panic!("Gc::deref: cannot dereference a null Gc");
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::deref: cannot dereference a dead, dropping, or under construction Gc"
        );
        &(*gc_box_ptr).value
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GC 實現中的一致性很重要。`Clone` 和 `try_deref` 都檢查 null，但 `Deref` 沒有，這是一個不一致的 API 設計，可能導致程式崩潰或 UB。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。解引用 null 指標在 Rust 中是 undefined behavior，即使在 unsafe 塊中也不應該發生。修復應該 panic 以提供明確的錯誤訊息。

**Geohot (Exploit 觀點):**
攻擊者可以透過構造 null Gc 並觸發解引用來造成程式崩潰（DoS）。雖然不太可能直接造成記憶體損壞，但這是一個可利用的穩定觸發點。

---

## Resolution (2026-03-02)

**Outcome:** Already fixed.

The fix was applied in a prior commit. The current `Gc::deref` implementation in `ptr.rs` (lines 1523–1536) correctly checks for null before dereferencing:

```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::deref: cannot dereference a null Gc");
    let gc_box_ptr = ptr.as_ptr();
    // ...
}
```

Behavior now matches `Clone` and `try_deref` as described in the issue.
