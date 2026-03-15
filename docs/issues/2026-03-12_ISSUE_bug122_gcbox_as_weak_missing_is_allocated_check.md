# [Bug]: GcBox::as_weak() 缺少 is_allocated 檢查導致 slot sweep 後潜在 UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 函數標記為 `#[allow(dead_code)]`，目前未被調用 |
| **Severity (嚴重程度)** | Medium | 如果啟用可能導致 slot sweep 後的 UAF |
| **Reproducibility (復現難度)** | Medium | 需啟用函數並构造 concurrent sweep 場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::as_weak()`, `ptr.rs:1511-1533`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcBox::as_weak()` 在調用 `inc_weak()` 後應該檢查 slot 是否已被 sweep。如果 slot 被 sweep，應該回傳 null weak reference 或 panic。

### 實際行為 (Actual Behavior)

函數調用 `inc_weak()` 增加 weak count，但**沒有**檢查 `is_allocated`。如果 slot 在 `inc_weak()` 之後被 lazy sweep 回收，可能會返回一個指向已釋放記憶體的 weak reference。

### 程式碼位置

`ptr.rs` 第 1511-1533 行：
```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    // ... 省略指標驗證 ...
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.is_under_construction()
            || gc_box.has_dead_flag()
            || gc_box.dropping_state() != 0
        {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*ptr.as_ptr()).inc_weak();  // <-- 調用 inc_weak
        // BUG: 缺少 is_allocated 檢查！
    }
    GcBoxWeakRef {
        ptr: AtomicNullable::new(ptr),
    }
}
```

### 對比：正確的實現模式

`Gc::downgrade()` (ptr.rs:1473-1481) 正確地檢查了 is_allocated：
```rust
(*gc_box_ptr).inc_weak();

if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*gc_box_ptr).dec_weak();
        panic!("Gc::downgrade: slot was swept during downgrade");
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `GcBox::as_weak()` 函數中：
1. 函數檢查了 `is_under_construction()`, `has_dead_flag()`, `dropping_state()`
2. 調用 `inc_weak()` 增加 weak count
3. **但是缺少** `is_allocated` 檢查

這與其他類似函數（如 `Gc::downgrade()`, `GcBoxWeakRef::clone()`）的實現不一致，這些函數都會在調用 `inc_weak()` 後檢查 `is_allocated`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

由於函數目前標記為 `#[allow(dead_code)]`，需要先啟用它才能測試。理論上的攻擊序列：
1. 呼叫 `GcBox::as_weak()` 獲得 weak reference
2. 在 inc_weak() 後、返回前觸發 lazy sweep
3. 使用返回的 weak reference 可能訪問已釋放的記憶體

```rust
// 理論 PoC（需要啟用 as_weak 函數）
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data { value: i32 }

let gc = Gc::new(Data { value: 42 });
let weak = gc.as_weak(); // BUG: 可能返回指向已釋放 slot 的 weak ref
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_weak()` 後添加 `is_allocated` 檢查，與 `Gc::downgrade()` 保持一致：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    let Some(ptr) = ptr.as_option() else {
        return GcBoxWeakRef {
            ptr: AtomicNullable::null(),
        };
    };
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.is_under_construction()
            || gc_box.has_dead_flag()
            || gc_box.dropping_state() != 0
        {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*ptr.as_ptr()).inc_weak();

        // 修復：添加 is_allocated 檢查
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                (*ptr.as_ptr()).dec_weak();
                return GcBoxWeakRef {
                    ptr: AtomicNullable::null(),
                };
            }
        }
    }
    GcBoxWeakRef {
        ptr: AtomicNullable::new(ptr),
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 slot reuse 問題。lazy sweep 可能在任何時間點回收 slot，如果沒有 `is_allocated` 檢查，返回的 weak reference 可能指向無效記憶體。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB，但可能導致記憶體安全问题。函數應與其他類似函數保持一致的檢查模式。

**Geohot (Exploit 攻擊觀點):**
如果函數被啟用，攻擊者可以構造 concurrent 場景在 inc_weak 和返回之間觸發 sweep，導致 use-after-free。

---

## Resolution (2026-03-13)

**Outcome:** Fixed.

Added `is_allocated` check after `inc_weak()` in `Gc::as_weak()` (ptr.rs), matching the pattern used in `Gc::downgrade()`. If the slot was swept during the operation, the function now calls `dec_weak()` and returns a null `GcBoxWeakRef` instead of a dangling reference.

---

## 備註

- 此函數標記為 `#[allow(dead_code)]`，目前未被使用
- 這是典型的「被低估的 bug」- 程式碼看起來合理，但與其他類似函數不一致
- 修復應該參考 `Gc::downgrade()` 的實現模式