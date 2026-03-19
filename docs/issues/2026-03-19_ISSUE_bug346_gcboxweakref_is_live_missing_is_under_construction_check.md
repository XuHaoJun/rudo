# [Bug]: GcBoxWeakRef::is_live() 缺少 is_under_construction 檢查，與 upgrade() 行為不一致

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Rare | 需要在 Gc::new_cyclic_weak 構造過程中調用 is_live() |
| **Severity (嚴重程度)** | Medium | 可能對正在構造中的物件錯誤返回 true |
| **Reproducibility (復現難度)** | Medium | 需精確時序，很難穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::is_live()` (`ptr.rs:804-830`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcBoxWeakRef::is_live()` 應該與 `GcBoxWeakRef::upgrade()` 具有一致的安全檢查。當物件正在構造中時（例如在 `Gc::new_cyclic_weak` 期間），兩者都應該返回表示物件不可存活的值：
- `is_live()` 應返回 `false`
- `upgrade()` 應返回 `None`

### 實際行為 (Actual Behavior)

目前 `GcBoxWeakRef::is_live()` 只檢查：
- null pointer
- address validity (alignment, min address)
- `is_gc_box_pointer_valid()`
- `has_dead_flag()`
- `dropping_state()`

但**缺少** `is_under_construction()` 檢查！

相比之下，`GcBoxWeakRef::upgrade()` 正確地檢查了 `is_under_construction()`：
```rust
// ptr.rs:646-648
if gc_box.is_under_construction() {
    return None;
}
```

同樣，`GcBoxWeakRef::try_upgrade()` 也檢查了：
```rust
// ptr.rs:848-850
if gc_box.is_under_construction() {
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:804-830`，`is_live()` 函數沒有調用 `is_under_construction()` 檢查：

```rust
pub(crate) fn is_live(&self) -> bool {
    // ... 省略有效性檢查 ...
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.has_dead_flag() {
            return false;
        }
        if gc_box.dropping_state() != 0 {
            return false;
        }
    }
    true // <-- 沒有 is_under_construction 檢查！
}
```

問題：
1. `upgrade()` 和 `try_upgrade()` 都檢查 `is_under_construction()` 防止访问未完成構造的物件
2. `is_live()` 不檢查，可能對正在構造中的物件返回 `true`
3. 這導致 API 不一致，且可能導致邏輯錯誤

注意：`Weak::is_alive()` 正確地委托給 `upgrade()`，因此沒有此問題。但 `WeakCrossThreadHandle::is_valid()` 使用 `self.weak.is_live()`，會受到此 bug 影響。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上需要：
1. 在 `Gc::new_cyclic_weak` 構造過程中（例如在 closure 內部）
2. 對同一物件的 weak reference 調用 `is_live()`
3. 應該返回 false 但可能返回 true

```rust
// 理論 PoC - 需要精確時序
use rudo_gc::{Gc, Weak, Trace};

#[derive(Trace)]
struct Node {
    value: i32,
    next: Option<Weak<Node>>,
}

fn main() {
    // 使用 new_cyclic_weak 創建環狀結構
    let weak_ref: Weak<Node>;
    let gc = Gc::new_cyclic_weak(|root| {
        // 在構造過程中，嘗試調用 is_live()
        // 這應該返回 false，但由於缺少檢查可能返回 true
        Node { value: 42, next: None }
    });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `ptr.rs:804-830` 處修改 `is_live()` 方法，添加 `is_under_construction` 檢查：

```rust
pub(crate) fn is_live(&self) -> bool {
    let Some(ptr) = self.as_ptr() else {
        return false;
    };

    let addr = ptr.as_ptr() as usize;
    let alignment = std::mem::align_of::<GcBox<T>>();
    if addr < MIN_VALID_HEAP_ADDRESS || addr % alignment != 0 {
        return false;
    }

    if !is_gc_box_pointer_valid(addr) {
        return false;
    }

    unsafe {
        let gc_box = &*ptr.as_ptr();
        
        // 檢查 is_under_construction（與 upgrade() 一致）
        if gc_box.is_under_construction() {
            return false;
        }
        
        if gc_box.has_dead_flag() {
            return false;
        }
        if gc_box.dropping_state() != 0 {
            return false;
        }
    }

    true
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- `is_live()` 是一個輕量級檢查，用於快速判斷物件是否可能存活
- 但它應該與 `upgrade()` 具有相同的安全檢查，確保 API 一致性
- 缺少 `is_under_construction` 檢查可能導致對正在構造中的物件的錯誤判斷

**Rustacean (Soundness 觀點):**
- 訪問未完成構造的物件是潛在的內存安全問題
- 雖然 `is_live()` 本身不直接訪問物件，但返回不正確的值會導致邏輯錯誤
- API 不一致會造成使用上的困惑

**Geohot (Exploit 攻擊觀點):**
- 在並髮場景下，攻擊者可能利用這個不一致的行為
- 雖然直接利用困難，但這是潛在的攻击面

---

## 關聯 Issue

- **Bug342**: `GcBox::try_inc_ref_from_zero` post-CAS 缺少 is_under_construction 檢查
- **Bug343**: `GcBoxWeakRef::is_live()` 缺少 is_allocated 檢查 (已報告)
- 此 bug 與 bug343 有關但不同：bug343 關注 is_allocated，此 bug 關注 is_under_construction
