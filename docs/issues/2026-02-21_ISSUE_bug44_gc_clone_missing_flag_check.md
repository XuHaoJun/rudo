# [Bug]: Gc::clone() 缺少 has_dead_flag 和 dropping_state 檢查導致異常行為

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當物件正在被 drop 或被標記為 dead 時嘗試 clone |
| **Severity (嚴重程度)** | High | 可能導致物件復活或參考計數錯誤 |
| **Reproducibility (復現難度)** | Medium | 需要精確時序，但在壓力測試下可穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::clone()`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Gc::clone()` 應該與 `Gc::deref()` 具有相同的安全檢查，確保物件不是 dead 或正在被 drop。

### 實際行為 (Actual Behavior)
`Gc::clone()` 完全沒有檢查 `has_dead_flag()` 或 `dropping_state()`，直接遞增 ref_count。這與 `Deref` 和 `try_deref` 的行為不一致。

此外，`try_clone` 只檢查 `has_dead_flag()` 但不檢查 `dropping_state()`，且其實現會調用有問題的 `clone()` 方法。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:1295-1314`

`Gc::clone()` 的實現：
```rust
impl<T: Trace> Clone for Gc<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return Self { /* ... */ };
        }

        let gc_box_ptr = ptr.as_ptr();

        // Increment reference count
        // SAFETY: Pointer is valid (not null)
        unsafe {
            (*gc_box_ptr).inc_ref();  // ✗ 沒有檢查 has_dead_flag() 或 dropping_state()!
        }

        Self { /* ... */ }
    }
}
```

對比 `Deref` 的實現（lines 1282-1292）：
```rust
impl<T: Trace> Deref for Gc<T> {
    fn deref(&self) -> &Self::Target {
        let ptr = self.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        unsafe {
            assert!(
                !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
                "Gc::deref: cannot dereference a dead Gc"
            );
            &(*gc_box_ptr).value
        }
    }
}
```

以及 `try_deref`（lines 1052-1063）：
```rust
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
            return None;
        }
        Some(&(*gc_box_ptr).value)
    }
}
```

**不一致問題：**
1. `Deref::deref()` - 檢查 both flags，否則 panic
2. `try_deref()` - 檢查 both flags，返回 None
3. `Clone::clone()` - **沒有檢查任何 flag！**
4. `try_clone()` - 只檢查 `has_dead_flag()`，不檢查 `dropping_state()`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_gc_clone_dead_object() {
    // Create a Gc and get another reference
    let gc1 = Gc::new(Data { value: 42 });
    let gc2 = gc1.clone();
    
    // Force the object to be marked as dead
    // This would require internal API access or specific GC trigger
    
    // Try to clone - this should fail but doesn't check flags
    let gc3 = gc2.clone();  // No check performed!
    
    // The clone succeeds even if the object is dead/dropping
}
```

更直接的方式是通過內部測試：
```rust
#[test]
fn test_clone_consistency_with_deref() {
    let gc = Gc::new(Data { value: 42 });
    let ptr = gc.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    
    // Manually set dead flag (for testing)
    unsafe {
        (*gc_box_ptr).set_dead_flag();
    }
    
    // This will panic with "cannot dereference a dead Gc"
    // let _ = *gc;
    
    // But clone will succeed! This is inconsistent.
    let _ = gc.clone();  // Should also panic or return None!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Clone::clone()` 中添加 flag 檢查：

```rust
impl<T: Trace> Clone for Gc<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return Self {
                ptr: AtomicNullable::null(),
                _marker: PhantomData,
            };
        }

        let gc_box_ptr = ptr.as_ptr();

        // SAFETY: Pointer is valid (not null)
        // Check flags before incrementing ref_count
        unsafe {
            assert!(
                !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
                "Gc::clone: cannot clone a dead or dropping Gc"
            );
            (*gc_box_ptr).inc_ref();
        }

        Self {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
            _marker: PhantomData,
        }
    }
}
```

同時修復 `try_clone()` 以檢查 both flags：

```rust
pub fn try_clone(gc: &Self) -> Option<Self> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        // Check BOTH flags now
        if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
            return None;
        }
    }
    Some(gc.clone())  // Now clone() also checks, so this is safe
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Debvig (GC 架構觀點):**
在 reference counting GC 中，clone 操作必須確保物件處於有效狀態。如果允許對 dead 或 dropping 狀態的物件遞增 ref_count，會導致：1. 物件被錯誤地「復活」2. ref_count 與實際的強引用數量不一致
3. 可能導致 double-free 或 use-after-free

**Rustacean (Soundness 觀點):**
這是一個 API 一致性問題。`Clone` 與 `Deref` 的行為不一致會造成混淆：- `Deref` 會 panic 如果物件是 dead- `try_deref` 返回 None 如果物件是 dead- 但 `Clone` 沒有任何檢查！

這違反了 Rust 的 "最小驚訝" 原則。

**Geohot (Exploit 觀點):**
這個 bug 可以被利用：1. 構造一個即將被 drop 的 Gc 物件
2. 在 dropping_state 設置後、ref_count 歸零前，呼叫 clone()
3. 物件被錯誤地增加 ref_count，導致：
   - 本該被釋放的記憶體繼續存在
   - 可能在後續造成記憶體洩漏
   - 或造成 use-after-free 如果記憶體被重新分配

---

**Resolution:** Added `assert!(!has_dead_flag() && dropping_state() == 0)` to `Gc::clone()` before `inc_ref()`, matching `Deref` semantics. Added `dropping_state() != 0` check to `Gc::try_clone()` alongside `has_dead_flag()`, matching `try_deref`.
