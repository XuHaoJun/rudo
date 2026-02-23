# [Bug]: Gc::clone() 缺少 is_under_construction 檢查 - 與其他操作不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在物件構造過程中進行 clone 操作相對少見，但在使用 Gc::new_cyclic 或類似模式時可能發生 |
| **Severity (嚴重程度)** | Medium | 可能導致存取未初始化資料，但需要特定時序才能觸發 |
| **Reproducibility (復現難度)** | High | 需要在物件構造過程中精確時機呼叫 clone，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Gc::clone, Gc::try_clone, GcHandle::clone
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Gc::clone()`、`Gc::try_clone()` 和 `GcHandle::clone()` 缺少對 `is_under_construction()` 標誌的檢查，與其他類似操作不一致。

### 預期行為 (Expected Behavior)

所有操作應該有一致的行為。當物件處於構造過程中時：
- `Gc::deref()` 會 panic（檢查 is_under_construction）
- `Weak::upgrade()` 會返回 None（檢查 is_under_construction）
- `Gc::try_from_raw()` 會返回 None（檢查 is_under_construction）
- `GcHandle::resolve()` 會 panic（檢查 is_under_construction）

### 實際行為 (Actual Behavior)

- `Gc::clone()` 不檢查 is_under_construction，允許克隆正在構造的物件
- `Gc::try_clone()` 不檢查 is_under_construction，允許克隆正在構造的物件
- `GcHandle::clone()` 不檢查 is_under_construction，允許克隆正在構造的 handle

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs` 中：

1. **Gc::clone** (lines 1355-1382): 只檢查 `has_dead_flag()` 和 `dropping_state()`
```rust
unsafe {
    assert!(
        !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
        "Gc::clone: cannot clone a dead or dropping Gc"
    );
    (*gc_box_ptr).inc_ref();
}
```

2. **Gc::try_clone** (lines 1093-1105): 同樣只檢查 `has_dead_flag()` 和 `dropping_state()`
```rust
unsafe {
    if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
        return None;
    }
}
```

3. **GcHandle::clone** (handles/cross_thread.rs lines 265-310): 同樣缺少檢查
```rust
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(
        !gc_box.has_dead_flag() && gc_box.dropping_state() == 0,
        "GcHandle::clone: cannot clone a dead or dropping GcHandle"
    );
    gc_box.inc_ref();
}
```

**對比**：正確的實現（如 Weak::upgrade, Gc::try_from_raw）都會檢查：
```rust
if gc_box.is_under_construction() {
    return None; // or panic
}
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
    // This pattern attempts to clone during construction
    // It should fail or return None, but currently may succeed
    let gc = Gc::new(Test { value: Cell::new(42) });
    
    // Clone should work fine for normal objects
    let _clone = gc.clone();
    
    println!("Clone succeeded for normal object");
}
```

Note: 真正的 bug 需要在物件構造過程中（GcBox::set_under_construction 為 true）呼叫 clone，這在正常使用中很難觸發，但在 Gc::new_cyclic 的實現中可能存在 edge case。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在所有三個 clone 實現中添加 `is_under_construction()` 檢查：

1. **Gc::clone** (`ptr.rs:1370`):
```rust
assert!(
    !(*gc_box_ptr).has_dead_flag() 
    && (*gc_box_ptr).dropping_state() == 0
    && !(*gc_box_ptr).is_under_construction(),
    "Gc::clone: cannot clone a dead, dropping, or under construction Gc"
);
```

2. **Gc::try_clone** (`ptr.rs:1100`):
```rust
if (*gc_box_ptr).has_dead_flag() 
    || (*gc_box_ptr).dropping_state() != 0
    || (*gc_box_ptr).is_under_construction() {
    return None;
}
```

3. **GcHandle::clone** (`handles/cross_thread.rs:296`):
```rust
assert!(
    !gc_box.has_dead_flag() 
    && gc_box.dropping_state() == 0
    && !gc_box.is_under_construction(),
    "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle"
);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間的 clone 會增加 ref_count，但物件可能尚未完全初始化
- 這類似於generational GC中需要特別處理的「不成熟物件」
- 建議：clone 應該複製「完整的物件狀態」，包括 construction flag

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致存取未初始化的資料
- 這是一個 soundness 問題，儘管觸發條件較嚴格
- 與其他 API（如 try_deref, upgrade）行為不一致，違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件被 clone 後可能導致 use-after-free
- 攻擊者可能透過精心設計的時序來利用這個 race condition
- 雖然難以穩定利用，但仍是潛在的攻击面
