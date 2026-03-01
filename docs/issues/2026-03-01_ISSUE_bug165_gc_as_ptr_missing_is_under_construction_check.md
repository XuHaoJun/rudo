# [Bug]: Gc::as_ptr 缺少 is_under_construction 檢查 - 導致可能存取未初始化資料

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | as_ptr 是低層 API，一般開發者不會直接呼叫 |
| **Severity (嚴重程度)** | High | 可能存取部分初始化的資料，導致 UB |
| **Reproducibility (重現難度)** | High | 需要在物件構造過程中精確時機呼叫 as_ptr，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::as_ptr()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Gc::as_ptr()` 應該檢查 `is_under_construction()` 標誌，與其他類似方法（如 `Gc::ref_count()`, `Gc::weak_count()`, `Gc::downgrade()`, `Gc::try_clone()`, `Gc::try_downgrade()`）的行為一致。

### 實際行為 (Actual Behavior)

`Gc::as_ptr()` 直接存取 `GcBox` 的 value 欄位而不檢查 `is_under_construction()`，導致：
- 物件構造過程中呼叫 `as_ptr()` 可能存取未初始化的資料
- 這與其他公開 API 的行為不一致

此問題與以下已記錄的 bug 為同一系列問題：
- Bug 89: Gc::clone 缺少 is_under_construction 檢查
- Bug 92: Gc::downgrade 缺少 is_under_construction 檢查
- Bug 95: Gc::ref_count/weak_count 缺少 is_under_construction 檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:1211-1216`

```rust
pub fn as_ptr(&self) -> *const T {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // SAFETY: ptr is not null (checked in callers), and ptr is valid
    unsafe { std::ptr::addr_of!((*gc_box_ptr).value) }
}
```

**對比**：正確的實現（如 `Gc::ref_count()`）都會檢查：
```rust
assert!(
    !(*gc_box_ptr).has_dead_flag()
        && (*gc_box_ptr).dropping_state() == 0
        && !(*gc_box_ptr).is_under_construction(),
    "Gc::ref_count: cannot get ref_count of a dead, dropping, or under construction Gc"
);
```

**其他類似方法也缺少檢查**：
- `Gc::internal_ptr()` (line 1219)
- `Gc::ptr_eq()` (line 1224)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::cell::Cell;

#[derive(Trace)]
struct Test {
    value: Cell<i32>,
}

// Gc::as_ptr is a low-level API, hard to trigger directly
// The bug would manifest if some code calls as_ptr on a Gc
// that's still under construction (e.g., during Gc::new_cyclic_weak)
```

Note: 真正的 bug 需要在物件構造過程中（GcBox::set_under_construction 為 true）呼叫 as_ptr，這在正常使用中很難觸發。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Gc::as_ptr()` 添加 `is_under_construction()` 檢查：

```rust
pub fn as_ptr(&self) -> *const T {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::as_ptr: cannot get ptr of a dead Gc");
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::as_ptr: cannot get ptr of a dead, dropping, or under construction Gc"
        );
        std::ptr::addr_of!((*gc_box_ptr).value)
    }
}
```

同樣地，也應該為 `internal_ptr()` 和 `ptr_eq()` 添加檢查。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間存取 value 可能讀取到部分初始化的資料
- 這類似於 generational GC 中需要特別處理的「不成熟物件」
- as_ptr 應該先驗證物件已完全構造完成

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致存取未初始化記憶體，這是 UB
- 與其他 API（如 ref_count, weak_count, downgrade）行為不一致
- 違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件被 as_ptr 後可能導致資訊洩露
- 攻擊者可能透過精心設計的時序來讀取未初始化的記憶體
- 這是一個潛在的攻擊面

---

## 關聯 Issue

- bug89: Gc::clone 缺少 is_under_construction 檢查
- bug92: Gc::downgrade 缺少 is_under_construction 檢查
- bug95: Gc::ref_count/weak_count 缺少 is_under_construction 檢查

---

## 修復紀錄

### 2026-03-01
**修復方式**: 在 `ptr.rs` 中為 `as_ptr()`, `internal_ptr()`, 和 `ptr_eq()` 添加了與其他 API 一致的檢查：
- null 檢查
- `has_dead_flag()` 檢查
- `dropping_state()` 檢查  
- `is_under_construction()` 檢查

現在這些函數的行為與 `Gc::ref_count()`, `Gc::weak_count()`, `Gc::downgrade()` 等其他 API 一致。
