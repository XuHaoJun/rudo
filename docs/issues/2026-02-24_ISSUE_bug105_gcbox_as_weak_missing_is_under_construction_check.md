# [Bug]: GcBox::as_weak() 缺少 is_under_construction 檢查 - 內部方法缺少一致性檢查

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | GcBox::as_weak 是內部方法，一般開發者不會直接調用 |
| **Severity (嚴重程度)** | Medium | 可能導致為構造中的物件增加 weak count，導致記憶體管理不一致 |
| **Reproducibility (復現難度)** | High | 需要在物件構造過程中精確時機呼叫 as_weak，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::as_weak()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcBox::as_weak()` 應該檢查 `is_under_construction()` 標誌，與其他類似方法（如 `Gc::clone()`, `Weak::upgrade()`, `Gc::downgrade()`）的行為一致。

### 實際行為 (Actual Behavior)

`GcBox::as_weak()` 直接調用 `inc_weak()` 而不檢查 `is_under_construction()`，導致：
- 物件構造過程中呼叫 `as_weak()` 會錯誤地增加 weak count
- 這與其他公開 API 的行為不一致

此問題與以下已記錄的 bug 為同一系列問題：
- Bug 89: Gc::clone 缺少 is_under_construction 檢查 (已修復)
- Bug 92: Gc::downgrade 缺少 is_under_construction 檢查
- Bug 94: Gc::deref/try_deref 缺少 is_under_construction 檢查
- Bug 95: Gc::ref_count/weak_count 缺少 is_under_construction 檢查
- Bug 104: Weak::clone/GcBoxWeakRef::clone 缺少 is_under_construction 檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:382-389`

```rust
/// Create a weak reference to this `GcBox`.
#[allow(dead_code)]
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    // Increment the weak count to track this weak reference.
    // SAFETY: self is a valid GcBox pointer.
    unsafe {
        (*NonNull::from(self).as_ptr()).inc_weak();  // 缺少: is_under_construction() 檢查!
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

**對比**：正確的實現（如 `Gc::clone()`）都會檢查：
```rust
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
use std::cell::Cell;

#[derive(Trace)]
struct Test {
    value: Cell<i32>,
}

fn main() {
    // GcBox::as_weak is an internal method, so it's hard to trigger directly
    // The bug would manifest if some public API internally calls as_weak
    // on an object that's still under construction
}
```

Note: 真正的 bug 需要在物件構造過程中（GcBox::set_under_construction 為 true）呼叫 as_weak，這在正常使用中很難觸發。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBox::as_weak()` 添加 `is_under_construction()` 檢查：

```rust
/// Create a weak reference to this `GcBox`.
#[allow(dead_code)]
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    // Increment the weak count to track this weak reference.
    // SAFETY: self is a valid GcBox pointer.
    unsafe {
        let gc_box = &*NonNull::from(self).as_ptr();
        if gc_box.is_under_construction() {
            // Return a null weak reference if object is under construction
            return GcBoxWeakRef::new(NonNull::dangling());
        }
        gc_box.inc_weak();
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

或者，使用 assert 與其他方法保持一致：

```rust
/// Create a weak reference to this `GcBox`.
#[allow(dead_code)]
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    // Increment the weak count to track this weak reference.
    // SAFETY: self is a valid GcBox pointer.
    unsafe {
        let gc_box = &*NonNull::from(self).as_ptr();
        assert!(
            !gc_box.is_under_construction(),
            "GcBox::as_weak: cannot create weak ref for object under construction"
        );
        gc_box.inc_weak();
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間的 as_weak 會增加 weak_count，但物件可能尚未完全初始化
- 這類似於 generational GC 中需要特別處理的「不成熟物件」
- 建議：as_weak 應該複製「完整的物件狀態」，包括 construction flag

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致為構造中的物件增加 weak count
- 這是一個記憶體管理一致性問題
- 與其他 API（如 clone, downgrade, upgrade）行為不一致，違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件被 as_weak 後可能導致記憶體管理不一致
- 攻擊者可能透過精心設計的時序來利用這個 race condition
- 雖然難以穩定利用，但仍是潛在的攻击面

---

## 關聯 Issue

- bug89: Gc::clone 缺少 is_under_construction 檢查 (已修復)
- bug92: Gc::downgrade 缺少 is_under_construction 檢查
- bug94: Gc::deref/try_deref 缺少 is_under_construction 檢查
- bug95: Gc::ref_count/weak_count 缺少 is_under_construction 檢查
- bug104: Weak::clone/GcBoxWeakRef::clone 缺少 is_under_construction 檢查
