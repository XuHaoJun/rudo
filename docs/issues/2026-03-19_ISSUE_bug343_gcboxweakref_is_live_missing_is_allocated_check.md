# [Bug]: GcBoxWeakRef::is_live() 缺少 is_allocated 檢查，與 upgrade() 行為不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 slot 被 sweep 回收後調用 is_live() |
| **Severity (嚴重程度)** | Medium | 可能返回true但物件實際已被回收並重用 |
| **Reproducibility (復現難度)** | Medium | 需要並髮場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::is_live()` (`ptr.rs:800-830`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcBoxWeakRef::is_live()` 應該與 `GcBoxWeakRef::upgrade()` 具有一致的行為。當 slot 被 sweep 回收並重新分配時，兩者都應該返回表示物件不可存活的值：
- `is_live()` 應返回 `false`
- `upgrade()` 應返回 `None`

### 實際行為 (Actual Behavior)

目前 `GcBoxWeakRef::is_live()` 只檢查：
- null pointer
- address validity (alignment, min address)
- `has_dead_flag()`
- `dropping_state()`

但**缺少** `is_allocated()` 檢查！

相比之下，`GcBoxWeakRef::upgrade()` 正確地檢查了 `is_allocated()`：
```rust
// ptr.rs:671-678
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return None; // 正確返回 None
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:800-830`，`is_live()` 函數沒有調用 `is_allocated()` 檢查：

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
    true // <-- 沒有 is_allocated 檢查！
}
```

問題：
1. `upgrade()` 檢查 `is_allocated()` 防止返回已回收的 slot
2. `is_live()` 不檢查，可能對已回收的 slot 返回 `true`
3. 這導致 API 不一致

注意：bug122 修復了 `dropping_state()` 檢查，bug320 提及此問題，但沒有創建獨立的 issue 來修復 `is_live()` 函數本身。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上需要：
1. 分配一個 GcBoxWeakRef
2. 觸發 GC 回收該 slot（ref_count=0, weak_count=0）
3. 該 slot 被 lazy sweep 回收並重新分配給新物件
4. 調用 `is_live()` - 應該返回 false 但可能返回 true

```rust
// 理論 PoC
use rudo_gc::{Gc, Weak, Trace, collect_full};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = gc.downgrade();
    // ... 理論上需要觸發 slot 回收和重用 ...
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `ptr.rs:800-830` 處修改 `is_live()` 方法，添加 `is_allocated` 檢查：

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
        // 檢查 is_allocated（與 upgrade() 一致）
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                return false;
            }
        }

        let gc_box = &*ptr.as_ptr();
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
- 缺少 `is_allocated` 可能導致對已回收 slot 的錯誤判斷

**Rustacean (Soundness 觀點):**
- 這不是 soundness 問題，因爲 `is_live()` 本身就是一個不確定的檢查
- 但 API 不一致會造成使用上的困惑，可能導致邏輯錯誤

**Geohot (Exploit 攻擊觀點):**
- 在並髮場景下，攻擊者可能利用這個不一致的行為
- 雖然直接利用困難，但這是潛在的攻击面

---

## 關聯 Issue

- **Bug122**: `GcBoxWeakRef::is_live()` 缺少 dropping_state 檢查 (已修復)
- **Bug242**: `GcBoxWeakRef::try_upgrade` 缺少 is_allocated 檢查 (已修復)
- **Bug320**: 提及 `is_live()` 缺少 is_allocated，但未單獨修復
