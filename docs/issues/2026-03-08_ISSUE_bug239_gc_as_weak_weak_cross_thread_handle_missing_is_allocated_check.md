# [Bug]: Gc::as_weak 和 Gc::weak_cross_thread_handle 缺少 is_allocated 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 GC 收集期間同時執行 Weak 升級，時序要求嚴格 |
| **Severity (嚴重程度)** | High | 可能導致 weak count 增加到已回收的 slot，導致記憶體洩漏或損壞 |
| **Reproducibility (復現難度)** | High | 需要精確時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr.rs`, `Gc::as_weak()`, `Gc::weak_cross_thread_handle()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`as_weak()` 和 `weak_cross_thread_handle()` 應該遵循與 `downgrade()` 相同的模式：在調用 `inc_weak()` 後檢查 `is_allocated`，以防止對已回收的 slot 錯誤增加 weak count。

### 實際行為 (Actual Behavior)
這兩個方法在調用 `inc_weak()` 後**沒有**檢查 `is_allocated`。如果 slot 在状态检查和 `inc_weak()` 调用之间被 lazy sweep 回收并重用，weak count 会被错误地增加到一个完全不同的对象上。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **正確模式（`downgrade`）**：在 `ptr.rs` 第 1473 行調用 `inc_weak()`，然後在第 1475-1481 行檢查 `is_allocated`，如果 slot 已被回收則 panic
2. **錯誤模式（`as_weak`）**：在第 1528 行調用 `inc_weak()`，**但從未檢查** `is_allocated`
3. **錯誤模式（`weak_cross_thread_handle`）**：在第 1628 行調用 `gc_box.inc_weak()`，**但從未檢查** `is_allocated`

### 受影響的程式碼位置：
- `ptr.rs:1528` - `as_weak()` 方法
- `ptr.rs:1628` - `weak_cross_thread_handle()` 方法

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell};

#[derive(Trace)]
struct Data {
    cell: GcCell<i32>,
}

fn main() {
    // 1. 建立大量 Gc 物件並用 weak reference 保持
    let gc = Gc::new(Data { cell: GcCell::new(42) });
    let weak = gc.as_weak();
    
    // 2. 確保物件晉升到 old generation
    // ...
    
    // 3. 同時觸發大量分配和 GC 收集
    // 4. 這會創建時序窗口讓 lazy sweep 回收 weak 指向的 slot
    // 5. 如果時序正確，as_weak() 會在 slot 回收後但新對象分配前調用 inc_weak
    
    // 預期：應該檢測到 slot 被回收並返回 null Weak
    // 實際：會錯誤地增加新對象的 weak count
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `as_weak()` 和 `weak_cross_thread_handle()` 中添加 `is_allocated` 檢查，遵循 `downgrade()` 的模式：

```rust
// ptr.rs - as_weak() 修復
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    // ... 現有檢查 ...
    (*ptr.as_ptr()).inc_weak();

    // NEW: 添加 is_allocated 檢查
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            (*ptr.as_ptr()).dec_weak();
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
    }
    
    GcBoxWeakRef {
        ptr: AtomicNullable::new(ptr),
    }
}

// ptr.rs - weak_cross_thread_handle() 修復
pub fn weak_cross_thread_handle(&self) -> crate::handles::WeakCrossThreadHandle<T> {
    unsafe {
        // ... 現有檢查 ...
        gc_box.inc_weak();

        // NEW: 添加 is_allocated 檢查
        if let Some(idx) = crate::heap::ptr_to_object_index(self.as_non_null().as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(self.as_non_null().as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                gc_box.dec_weak();
                panic!("Gc::weak_cross_thread_handle: slot was swept during handle creation");
            }
        }
    }
    // ... 
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dydvig (GC 架構觀點):**
在 lazy sweep 實現中，slot 可以在任何時候被回收並重用。如果 weak reference 升級時沒有檢查 slot 的分配狀態，會導致：
1. Weak count 增加到錯誤的對象
2. 原始對象的 weak count 永遠不會歸零，導致記憶體洩漏
3. 新對象會有額外的 weak count，影響後續的 weak upgrade 行為

**Rustacean (Soundness 觀點):**
這是 API 不一致的問題。`downgrade()` 正確地檢查了 `is_allocated`，但 `as_weak()` 和 `weak_cross_thread_handle()` 卻遺漏了這個檢查。這種不一致性會導致難以預測的行為。

**Geohot (Exploit 觀點):**
如果攻擊者能控制時序，可能：
1. 通過精確的 memory layout 控制，讓被錯誤增加 weak count 的對象成為關鍵結構
2. 破壞 GC 的 weak reference 完整性假設

---

## Resolution (2026-03-14)

**Outcome:** Already fixed.

Both `Gc::as_weak()` and `Gc::weak_cross_thread_handle()` already have the `is_allocated` check after `inc_weak()` in the current implementation (`ptr.rs`):

- **as_weak()** (lines 1653–1660): After `inc_weak()`, checks `is_allocated`; if slot was swept, returns null `GcBoxWeakRef` without calling `dec_weak` (per bug133).
- **weak_cross_thread_handle()** (lines 1766–1773): After `inc_weak()`, asserts `is_allocated` with message "object slot was swept after inc_weak".

Behavior matches the suggested fix and is consistent with `Gc::downgrade()`. Duplicate of bug122 (Gc::weak_cross_thread_handle).
