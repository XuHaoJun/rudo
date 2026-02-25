# [Bug]: Gc::downgrade() 缺少 is_under_construction 檢查 - 與 Gc::clone() 行為不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在物件構造過程中進行 downgrade 操作相對少見，但在使用 Gc::new_cyclic 或類似模式時可能發生 |
| **Severity (嚴重程度)** | Medium | 可能導致存取未初始化資料，但需要特定時序才能觸發 |
| **Reproducibility (復現難度)** | High | 需要在物件構造過程中精確時機呼叫 downgrade，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Gc::downgrade, GcHandle::downgrade, Gc::weak_cross_thread_handle
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Gc::downgrade()`、`GcHandle::downgrade()` 和 `Gc::weak_cross_thread_handle()` 缺少對 `is_under_construction()` 標誌的檢查，與 Bug 89 中修復的 `Gc::clone()` 行為不一致。

### 預期行為 (Expected Behavior)

所有操作應該有一致的行為。當物件處於構造過程中時：
- `Gc::deref()` 會 panic（檢查 is_under_construction）
- `Weak::upgrade()` 會返回 None（檢查 is_under_construction）
- `Gc::clone()` 會 panic（檢查 is_under_construction） - Bug 89 已修復
- `Gc::downgrade()` 應該也會 panic（檢查 is_under_construction）

### 實際行為 (Actual Behavior)

- `Gc::downgrade()` 只檢查 `has_dead_flag()` 和 `dropping_state()`，不檢查 `is_under_construction()`
- `GcHandle::downgrade()` 同樣缺少檢查
- `Gc::weak_cross_thread_handle()` 同樣缺少檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs` 中：

1. **Gc::downgrade** (lines 1196-1210): 只檢查 `has_dead_flag()` 和 `dropping_state()`
```rust
pub fn downgrade(gc: &Self) -> Weak<T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::downgrade: cannot downgrade a dead Gc");
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::downgrade: Gc is dead or in dropping state"
        );
        (*gc_box_ptr).inc_weak();  // 缺少: is_under_construction() 檢查!
    }
    // ...
}
```

2. **Gc::weak_cross_thread_handle** (lines 1322-1339): 同樣缺少檢查
```rust
pub fn weak_cross_thread_handle(&self) -> crate::handles::WeakCrossThreadHandle<T> {
    unsafe {
        let gc_box = &*self.as_non_null().as_ptr();
        assert!(
            !gc_box.has_dead_flag() && gc_box.dropping_state() == 0,
            "Gc::weak_cross_thread_handle: cannot create handle for dead or dropping Gc"
        );
        gc_box.inc_weak();  // 缺少: is_under_construction() 檢查!
    }
    // ...
}
```

3. **GcHandle::downgrade** (handles/cross_thread.rs lines 248-262): 同樣缺少檢查
```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.has_dead_flag() && gc_box.dropping_state() == 0,
            "GcHandle::downgrade: object is dead or in dropping state"
        );
        gc_box.inc_weak();  // 缺少: is_under_construction() 檢查!
    }
    // ...
}
```

**對比**：正確的實現（如 Gc::clone 在 Bug 89 修復後）都會檢查：
```rust
assert!(
    !(*gc_box_ptr).has_dead_flag() 
    && (*gc_box_ptr).dropping_state() == 0
    && !(*gc_box_ptr).is_under_construction(),
    "Gc::downgrade: cannot downgrade a dead, dropping, or under construction Gc"
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
    // This pattern attempts to downgrade during construction
    // It should fail, but currently may succeed
    let gc = Gc::new(Test { value: Cell::new(42) });
    
    // Downgrade should work fine for normal objects
    let _weak = gc.downgrade();
    
    println!("Downgrade succeeded for normal object");
}
```

Note: 真正的 bug 需要在物件構造過程中（GcBox::set_under_construction 為 true）呼叫 downgrade，這在正常使用中很難觸發，但在 Gc::new_cyclic 的實現中可能存在 edge case。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在所有三個 downgrade 實現中添加 `is_under_construction()` 檢查：

1. **Gc::downgrade** (`ptr.rs:1201-1204`):
```rust
assert!(
    !(*gc_box_ptr).has_dead_flag() 
    && (*gc_box_ptr).dropping_state() == 0
    && !(*gc_box_ptr).is_under_construction(),
    "Gc::downgrade: cannot downgrade a dead, dropping, or under construction Gc"
);
```

2. **Gc::weak_cross_thread_handle** (`ptr.rs:1327-1330`):
```rust
assert!(
    !gc_box.has_dead_flag() 
    && gc_box.dropping_state() == 0
    && !gc_box.is_under_construction(),
    "Gc::weak_cross_thread_handle: cannot create handle for dead, dropping, or under construction Gc"
);
```

3. **GcHandle::downgrade** (`handles/cross_thread.rs:251-254`):
```rust
assert!(
    !gc_box.has_dead_flag() 
    && gc_box.dropping_state() == 0
    && !gc_box.is_under_construction(),
    "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 物件構造期間的 downgrade 會增加 weak_count，但物件可能尚未完全初始化
- 這類似於generational GC中需要特別處理的「不成熟物件」
- 建議：downgrade 應該複製「完整的物件狀態」，包括 construction flag

**Rustacean (Soundness 觀點):**
- 缺少檢查可能導致存取未初始化的資料
- 這是一個 soundness 問題，儘管觸發條件較嚴格
- 與其他 API（如 clone, try_deref, upgrade）行為不一致，違反最小驚訝原則

**Geohot (Exploit 觀點):**
- 在並髮環境中，構造中的物件被 downgrade 後可能導致 use-after-free
- 攻擊者可能透過精心設計的時序來利用這個 race condition
- 雖然難以穩定利用，但仍是潛在的攻击面

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

Added `is_under_construction()` check to all three locations (matching Gc::clone behavior from bug89):

1. **Gc::downgrade** (`ptr.rs`): assert now includes `&& !(*gc_box_ptr).is_under_construction()`
2. **Gc::weak_cross_thread_handle** (`ptr.rs`): assert now includes `&& !gc_box.is_under_construction()`
3. **GcHandle::downgrade** (`handles/cross_thread.rs`): assert now includes `&& !gc_box.is_under_construction()`
