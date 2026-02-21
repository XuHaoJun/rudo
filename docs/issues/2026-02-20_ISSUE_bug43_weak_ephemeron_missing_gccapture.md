# [Bug]: Weak<T> and Ephemeron<K,V> missing GcCapture implementation

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 GcCell 內使用 Weak 或 Ephemeron |
| **Severity (嚴重程度)** | Medium | 編譯期錯誤，導致 API 無法使用 |
| **Reproducibility (復現難度)** | Very High | 每次嘗試使用都會失敗 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` trait, `Weak<T>`, `Ephemeron<K,V>`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Weak<T>` and `Ephemeron<K,V>` do not implement the `GcCapture` trait, which prevents them from being used inside `GcCell` types that require `GcCapture` for SATB barriers.

### 預期行為 (Expected Behavior)
Should be able to use `Weak<T>` inside a `GcCell`:
```rust
#[derive(Trace, GcCapture)]
struct MyStruct {
    weak_ref: Weak<SomeType>,  // Should compile
}
```

### 實際行為 (Actual Behavior)
Compilation error - `GcCapture` is not implemented for `Weak<T>` or `Ephemeron<K,V>`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

Looking at `cell.rs`, we can see:
- `Gc<T>` implements `GcCapture` at line 377
- `Weak<T>` does NOT implement `GcCapture`
- `Ephemeron<K,V>` does NOT implement `GcCapture`

This is inconsistent - `Gc<T>` can be used inside `GcCell`, but `Weak<T>` cannot, even though both are GC pointer types.

The `GcCapture` trait is required by `GcCell::borrow_mut()` for SATB barrier recording. Without it, types containing `Weak` or `Ephemeron` cannot be used with `GcCell`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, cell::GcCapture, cell::GcCell, Weak};

#[derive(Trace, GcCapture)]
struct MyStruct {
    weak_ref: Weak<i32>,
}

fn main() {
    let cell = GcCell::new(MyStruct {
        weak_ref: Weak::new(),
    });
    
    let _ = cell.borrow_mut();  // This will fail to compile
}
```

Compilation error:
```
error[E0277]: the trait bound `Weak<i32>: GcCapture` is not satisfied
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `GcCapture` implementations for `Weak<T>` and `Ephemeron<K,V>` in `ptr.rs`:

1. For `Weak<T>`: Similar to `Gc<T>`, capture the internal pointer
2. For `Ephemeron<K,V>`: Capture both the key (as weak) and value (as strong)

```rust
// In ptr.rs, add:

unsafe impl<T: Trace + 'static> GcCapture for Weak<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Weak references don't keep objects alive, but they should still
        // be captured for SATB to track OLD->YOUNG references
        if let Some(gc) = self.upgrade() {
            let raw = gc.raw_ptr();
            if !raw.is_null() {
                unsafe {
                    let nn = NonNull::new_unchecked(raw.cast());
                    ptrs.push(nn);
                }
            }
        }
    }
}

unsafe impl<K: Trace + 'static, V: Trace + 'static> GcCapture for Ephemeron<K, V> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Capture value (strong reference)
        let raw = self.value.raw_ptr();
        if !raw.is_null() {
            unsafe {
                let nn = NonNull::new_unchecked(raw.cast());
                ptrs.push(nn);
            }
        }
        // Optionally capture key if alive
        if let Some(key) = self.key.upgrade() {
            let raw = key.raw_ptr();
            if !raw.is_null() {
                unsafe {
                    let nn = NonNull::new_unchecked(raw.cast());
                    ptrs.push(nn);
                }
            }
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The SATB barrier requires capturing all GC pointers that might have OLD->YOUNG references. `Weak<T>` pointers can hold references to old generation objects, and when upgraded, they create OLD->YOUNG references that need to be tracked. Similarly, `Ephemeron` has both weak key and strong value references that should be captured.

**Rustacean (Soundness 觀點):**
This is primarily a usability issue - the lack of `GcCapture` implementation prevents legitimate use cases. It's a compile-time error rather than a soundness issue, but it limits the API usability significantly.

**Geohot (Exploit 觀點):**
No direct exploit path here - this is a missing feature that causes compilation failures rather than memory safety issues.
