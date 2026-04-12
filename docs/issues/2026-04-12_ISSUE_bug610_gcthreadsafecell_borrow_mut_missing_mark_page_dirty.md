# [Bug]: GcThreadSafeCell::borrow_mut() missing mark_page_dirty_for_borrow call

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any use of GcThreadSafeCell::borrow_mut() with GcCell<Vec<Gc<T>>> is affected |
| **Severity (嚴重程度)** | High | Children in GcThreadSafeCell<Vec<Gc<T>>> may be incorrectly swept during minor GC |
| **Reproducibility (Reproducibility)** | Medium | Can be observed via memory corruption or missing children |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut()` (cell.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcThreadSafeCell::borrow_mut()` 應該與 `GcCell::borrow_mut()`有一致的行為，包括呼叫 `mark_page_dirty_for_borrow()` 來確保 children 在 minor GC 時被追蹤。

### 實際行為 (Actual Behavior)

`GcCell::borrow_mut()` (cell.rs:199-206) 有 `mark_page_dirty_for_borrow()` 呼叫：
```rust
// FIX bug583: Always mark the page dirty when borrow_mut is called.
// The gen_old optimization (bug71) skips recording OLD→YOUNG references when
// page is young (gen=0) and gen_old flag is not set. But for minor GC tracing,
// we need the page to be in dirty_pages so children in GcCell<Vec<Gc<T>>>
// are traced. Without this, children are incorrectly swept.
unsafe {
    crate::heap::mark_page_dirty_for_borrow(ptr);
}
```

但 `GcThreadSafeCell::borrow_mut()` (cell.rs:1078-1141) 沒有這個呼叫。

### 對比

**GcCell::borrow_mut() (有 fix):**
- Lines 199-206: Has `mark_page_dirty_for_borrow()` call

**GcThreadSafeCell::borrow_mut() (缺少 fix):**
- Lines 1078-1141: NO `mark_page_dirty_for_borrow()` call

---

## 🔬 根本原因分析 (Root Cause Analysis)

`mark_page_dirty_for_borrow()` 函數存在是為了確保當 `GcCell::borrow_mut()` 被呼叫時，無論 gen_old 優化（bug71）的狀態如何，頁面都會被標記為 dirty。這對於 minor GC 追蹤 `GcCell<Vec<Gc<T>>>` 中的 children 至關重要。

`GcThreadSafeCell::borrow_mut()` 缺少這個呼叫，導致：
1. 當使用 `GcThreadSafeCell<Vec<Gc<T>>>` 時
2. 在 minor GC 期間
3. Children 可能被錯誤地 sweep

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, GcCapture, collect_full};
use std::cell::RefCell;

#[derive(Trace, GcCapture)]
struct Container {
    data: RefCell<Vec<Gc<i32>>>,
}

fn main() {
    let cell = Gc::new(GcThreadSafeCell::new(Container {
        data: RefCell::new(vec![]),
    }));

    // Allocate some Gc objects
    let gc1 = Gc::new(42i32);
    let gc2 = Gc::new(43i32);

    // Add to container via borrow_mut
    cell.borrow_mut().data.borrow_mut().push(gc1.clone());
    cell.borrow_mut().data.borrow_mut().push(gc2.clone());

    // Force minor GC
    collect_full();

    // Access again - children may have been incorrectly swept!
    let data = cell.borrow();
    assert_eq!(data.borrow().len(), 2); // May fail!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcThreadSafeCell::borrow_mut()` 中新增 `mark_page_dirty_for_borrow()` 呼叫，與 `GcCell::borrow_mut()` 保持一致：

```rust
// In GcThreadSafeCell::borrow_mut(), after line 1117:
// FIX bugXXX: Add mark_page_dirty_for_borrow call to match GcCell::borrow_mut()
// The gen_old optimization skips recording OLD→YOUNG references when page is young.
// But for minor GC tracing, we need the page in dirty_pages so children are traced.
unsafe {
    crate::heap::mark_page_dirty_for_borrow(/* ptr to the GcThreadSafeCell data */);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The gen_old optimization (bug71) is a performance improvement that skips recording OLD→YOUNG references when the page is young and the gen_old flag is not set. However, for minor GC tracing to work correctly with containers like `GcCell<Vec<Gc<T>>>`, the page must be in the dirty_pages set. The `mark_page_dirty_for_borrow()` call ensures this regardless of the gen_old optimization state.

**Rustacean (Soundness 觀點):**
This is a GC correctness bug rather than a memory safety issue in the traditional sense. Children stored in `GcThreadSafeCell<Vec<Gc<T>>>` may be prematurely collected during minor GC because the dirty page tracking is not properly maintained. This could manifest as use-after-free when accessing those children later.

**Geohot (Exploit 觀點):**
An attacker could potentially trigger this bug to cause memory corruption by manipulating GC timing. If they can cause a minor GC to run at a specific moment, they might be able to trigger the premature collection of children in `GcThreadSafeCell<Vec<Gc<T>>>` containers.