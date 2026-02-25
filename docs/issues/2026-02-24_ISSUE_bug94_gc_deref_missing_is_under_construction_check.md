# [Bug]: Gc::deref() 和 Gc::try_deref() 缺少 is_under_construction 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在物件構造過程中進行 deref 操作相對少見，但在使用 Gc::new_cyclic 或類似模式時可能發生 |
| **Severity (嚴重程度)** | Medium | 可能導致存取未初始化資料，但需要特定時序才能觸發 |
| **Reproducibility (復現難度)** | High | 需要在物件構造過程中精確時機呼叫 deref，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Gc::deref, Gc::try_deref
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Gc::deref()` 和 `Gc::try_deref()` 缺少對 `is_under_construction()` 標誌的檢查。

### 預期行為 (Expected Behavior)

所有操作應該有一致的行為。當物件處於構造過程中時：
- `Gc::deref()` 應該 panic（檢查 is_under_construction）
- `Gc::try_deref()` 應該返回 None（檢查 is_under_construction）
- `Weak::upgrade()` 會返回 None（檢查 is_under_construction）
- `Gc::clone()` 會 panic（檢查 is_under_construction） - Bug 89 已修復
- `Gc::downgrade()` 應該也會 panic（檢查 is_under_construction） - Bug 92

### 實際行為 (Actual Behavior)

- `Gc::deref` 只檢查 `has_dead_flag()` 和 `dropping_state()`，不檢查 `is_under_construction()`
- `Gc::try_deref` 同樣缺少檢查

**注意**：Bug 92 錯誤地聲稱 `Gc::deref()` 會檢查 is_under_construction，但實際代碼中並沒有這個檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs` 中：

1. **Gc::deref** (lines 1345-1356): 只檢查 `has_dead_flag()` 和 `dropping_state()`
```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::deref: cannot dereference a dead Gc"
        );
        &(*gc_box_ptr).value  // 缺少: is_under_construction() 檢查!
    }
}
```

2. **Gc::try_deref** (lines 1076-1088): 同樣缺少檢查
```rust
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
            return None;  // 缺少: is_under_construction() 檢查!
        }
        Some(&(*gc_box_ptr).value)
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
    // This pattern attempts to deref during construction
    // It should fail, but currently may succeed
    let gc = Gc::new(Test { value: Cell::new(42) });
    
    // Deref should work fine for normal objects
    let _value = gc.value;
    
    println!("Dereference succeeded for normal object");
}
```

Note: 真正的 bug 需要在物件構造過程中（GcBox::set_under_construction 為 true）呼叫 deref，這在正常使用中很難觸發，但在 Gc::new_cyclic 的實現中可能存在 edge case。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `deref` 和 `try_deref` 實現中添加 `is_under_construction()` 檢查：

1. **Gc::deref** (`ptr.rs:1349-1352`):
```rust
assert!(
    !(*gc_box_ptr).has_dead_flag() 
    && (*gc_box_ptr).dropping_state() == 0
    && !(*gc_box_ptr).is_under_construction(),
    "Gc::deref: cannot dereference a dead, dropping, or under construction Gc"
);
```

2. **Gc::try_deref** (`ptr.rs:1083-1085`):
```rust
if (*gc_box_ptr).has_dead_flag() 
    || (*gc_box_ptr).dropping_state() != 0
    || (*gc_box_ptr).is_under_construction()
{
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間的 deref 會讀取未初始化的資料
- 這類似於generational GC中需要特別處理的「不成熟物件」
- 建議：deref 應該檢查 construction flag，確保物件已完全初始化

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致存取未初始化的資料
- 這是一個 soundness 問題，儘管觸發條件較嚴格
- 與其他 API（如 clone, try_clone, upgrade）行為不一致，違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件被 deref 後可能導致資訊洩露
- 攻擊者可能透過精心設計的時序來利用這個問題
- 雖然難以穩定利用，但仍是潛在的攻击面

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

1. Added `is_under_construction()` check to `Gc::deref` in `ptr.rs` — now asserts `!(*gc_box_ptr).is_under_construction()` alongside dead/dropping checks.
2. Added `is_under_construction()` check to `Gc::try_deref` in `ptr.rs` — now returns `None` when object is under construction.
3. Aligns behavior with `Gc::clone`, `Gc::downgrade`, and other Gc operations.
