# [Bug]: Weak::strong_count() 和 Weak::weak_count() 缺少 is_under_construction 檢查 - 與 Weak::upgrade 行為不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在物件構造過程中呼叫 strong_count/weak_count 相對少見 |
| **Severity (嚴重程度)** | Low | 可能返回不正確的計數，但較少在 construction 期間呼叫這些方法 |
| **Reproducibility (復現難度)** | High | 需要在物件構造過程中精確時機呼叫，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::strong_count`, `Weak::weak_count` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Weak::strong_count()` 和 `Weak::weak_count()` 缺少對 `is_under_construction()` 標誌的檢查，與 `Weak::upgrade()` 的行為不一致。

此問題與 Bug 95 (`Gc::ref_count`/`Gc::weak_count` 缺少檢查) 為同一系列問題，但這次是針對 `Weak` 類型。

### 預期行為 (Expected Behavior)

所有操作應該有一致的行為。當物件處於構造過程中時：
- `Weak::upgrade()` 返回 None（檢查 is_under_construction）
- `Weak::strong_count()` 應該返回 0（檢查 is_under_construction）
- `Weak::weak_count()` 應該返回 0（檢查 is_under_construction）

### 實際行為 (Actual Behavior)

- `Weak::strong_count` 只檢查 `has_dead_flag()` 和 `dropping_state()`，不檢查 `is_under_construction()`
- `Weak::weak_count` 同樣缺少檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs` 中：

1. **Weak::strong_count** (lines 1794-1812): 只檢查 `has_dead_flag()` 和 `dropping_state()`
```rust
pub fn strong_count(&self) -> usize {
    // ...
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 {
            0
        } else {
            gc_box.ref_count().get()  // 缺少: is_under_construction() 檢查!
        }
    }
}
```

2. **Weak::weak_count** (lines 1818-1834): 同樣缺少檢查
```rust
pub fn weak_count(&self) -> usize {
    // ...
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 {
            0
        } else {
            gc_box.weak_count()  // 缺少: is_under_construction() 檢查!
        }
    }
}
```

**對比**：正確的實現（如 `Weak::upgrade`）都會檢查：
```rust
if gc_box.is_under_construction() {
    return None;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace};
use std::cell::Cell;

#[derive(Trace)]
struct Test {
    value: Cell<i32>,
}

fn main() {
    let gc = Gc::new(Test { value: Cell::new(42) });
    let weak = Gc::downgrade(&gc);
    
    // 正常呼叫應該返回 1
    let count = weak.strong_count();
    println!("strong_count: {}", count);
    
    // Note: 真正的 bug 需要在物件構造過程中呼叫，
    // 這在正常使用中很難觸發，但在 Gc::new_cyclic 的實現中可能存在 edge case。
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `strong_count` 和 `weak_count` 實現中添加 `is_under_construction()` 檢查：

1. **Weak::strong_count** (`ptr.rs:1804-1811`):
```rust
unsafe {
    let gc_box = &*ptr.as_ptr();
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag() 
        || gc_box.dropping_state() != 0
    {
        0
    } else {
        gc_box.ref_count().get()
    }
}
```

2. **Weak::weak_count** (`ptr.rs:1828-1834`):
```rust
unsafe {
    let gc_box = &*ptr.as_ptr();
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag() 
        || gc_box.dropping_state() != 0
    {
        0
    } else {
        gc_box.weak_count()
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間讀取 weak count 會讀取未完全初始化的計數器
- 這類似於generational GC中需要特別處理的「不成熟物件」
- 建議：weak count 應該檢查 construction flag，確保物件已完全初始化

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致讀取不正確的計數值
- 這是一個一致性問題，儘管觸發條件較嚴格
- 與其他 API（如 Weak::upgrade）行為不一致，違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件的 weak count 被讀取後可能導致資訊洩露
- 攻擊者可能透過精心設計的時序來利用這個問題
- 雖然難以穩定利用，但仍是潛在的攻击面
