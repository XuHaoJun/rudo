# [Bug]: Gc::ref_count() 和 Gc::weak_count() 缺少 is_under_construction 檢查

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在物件構造過程中進行 ref_count/weak_count 操作相對少見，但在使用 Gc::new_cyclic 或類似模式時可能發生 |
| **Severity (嚴重程度)** | Medium | 可能導致讀取未初始化資料，但需要特定時序才能觸發 |
| **Reproducibility (復現難度)** | High | 需要在物件構造過程中精確時機呼叫 ref_count/weak_count，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Gc::ref_count, Gc::weak_count
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Gc::ref_count()` 和 `Gc::weak_count()` 缺少對 `is_under_construction()` 標誌的檢查。

此問題與 Bug 94 (Gc::deref/try_deref 缺少檢查) 和 Bug 92 (Gc::downgrade 缺少檢查) 為同一系列問題 - 多個 Gc 方法缺少一致的 `is_under_construction` 檢查。

### 預期行為 (Expected Behavior)

所有操作應該有一致的行為。當物件處於構造過程中時：
- `Gc::deref()` 應該 panic（檢查 is_under_construction） - Bug 94
- `Gc::try_deref()` 應該返回 None（檢查 is_under_construction） - Bug 94
- `Gc::clone()` 會 panic（檢查 is_under_construction） - Bug 89 已修復
- `Gc::downgrade()` 應該 panic（檢查 is_under_construction） - Bug 92
- `Gc::ref_count()` 應該 panic（檢查 is_under_construction）
- `Gc::weak_count()` 應該 panic（檢查 is_under_construction）
- `Weak::upgrade()` 會返回 None（檢查 is_under_construction）

### 實際行為 (Actual Behavior)

- `Gc::ref_count` 只檢查 `has_dead_flag()` 和 `dropping_state()`，不檢查 `is_under_construction()`
- `Gc::weak_count` 同樣缺少檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs` 中：

1. **Gc::ref_count** (lines 1140-1154): 只檢查 `has_dead_flag()` 和 `dropping_state()`
```rust
pub fn ref_count(gc: &Self) -> NonZeroUsize {
    // ...
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::ref_count: Gc is dead or in dropping state"
        );
        (*gc_box_ptr).ref_count()
    }
}
```

2. **Gc::weak_count** (lines 1161-1175): 同樣缺少檢查
```rust
pub fn weak_count(gc: &Self) -> usize {
    // ...
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::weak_count: Gc is dead or in dropping state"
        );
        (*gc_box_ptr).weak_count()
    }
}
```

**對比**：正確的實現（如 Gc::clone）都會檢查：
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
    // This pattern attempts to get ref_count during construction
    // It should fail, but currently may succeed
    let gc = Gc::new(Test { value: Cell::new(42) });
    
    // ref_count should work fine for normal objects
    let _count = Gc::ref_count(&gc);
    
    println!("ref_count succeeded for normal object");
}
```

Note: 真正的 bug 需要在物件構造過程中（GcBox::set_under_construction 為 true）呼叫 ref_count，這在正常使用中很難觸發，但在 Gc::new_cyclic 的實現中可能存在 edge case。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `ref_count` 和 `weak_count` 實現中添加 `is_under_construction()` 檢查：

1. **Gc::ref_count** (`ptr.rs:1148-1151`):
```rust
assert!(
    !(*gc_box_ptr).has_dead_flag() 
    && (*gc_box_ptr).dropping_state() == 0
    && !(*gc_box_ptr).is_under_construction(),
    "Gc::ref_count: cannot get ref_count of a dead, dropping, or under construction Gc"
);
```

2. **Gc::weak_count** (`ptr.rs:1169-1172`):
```rust
assert!(
    !(*gc_box_ptr).has_dead_flag() 
    && (*gc_box_ptr).dropping_state() == 0
    && !(*gc_box_ptr).is_under_construction(),
    "Gc::weak_count: cannot get weak_count of a dead, dropping, or under construction Gc"
);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間讀取 ref_count 會讀取未完全初始化的計數器
- 這類似於generational GC中需要特別處理的「不成熟物件」
- 建議：ref_count 應該檢查 construction flag，確保物件已完全初始化

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致讀取未初始化的資料
- 這是一個 soundness 問題，儘管觸發條件較嚴格
- 與其他 API（如 clone, try_clone, upgrade）行為不一致，違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件的 ref_count 被讀取後可能導致資訊洩露
- 攻擊者可能透過精心設計的時序來利用這個問題
- 雖然難以穩定利用，但仍是潛在的攻击面
