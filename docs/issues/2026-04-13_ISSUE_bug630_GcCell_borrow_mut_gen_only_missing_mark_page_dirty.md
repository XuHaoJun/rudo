# [Bug]: GcCell::borrow_mut_gen_only() 缺少 mark_page_dirty_for_borrow() - 導致minor GC時children被錯誤回收

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | 任何使用 `borrow_mut_gen_only()` 且類型包含 `Gc<T>` 的程式碼都會觸發 |
| **Severity (嚴重程度)** | `Critical` | 記憶體安全問題 - 可能導致 UAF |
| **Reproducibility (復現難度)** | `Medium` | 需要特定條件：generational barrier 啟用 + 年輕頁面 + 包含 Gc 的類型 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut_gen_only()` (cell.rs:272-282)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`borrow_mut_gen_only()` 應該在调用 `borrow_mut()` 之前始终调用 `mark_page_dirty_for_borrow()`，确保页面被添加到 dirty_pages，以便在 minor GC 期间追踪 children。

### 實際行為 (Actual Behavior)

`borrow_mut_gen_only()` 调用 `gc_cell_validate_and_barrier()` (当 generational_active 为 true 时)，但**不调用** `mark_page_dirty_for_borrow()`。

相比之下，`borrow_mut()` (cell.rs:199-206) **始终**调用 `mark_page_dirty_for_borrow()`。

### 程式碼差異

**borrow_mut() (lines 199-206):**
```rust
// FIX bug583: Always mark the page dirty when borrow_mut is called.
unsafe {
    crate::heap::mark_page_dirty_for_borrow(ptr);
}
// ... 然后调用 borrow_mut()
```

**borrow_mut_gen_only() (lines 272-282):**
```rust
pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T> {
    self.validate_thread_affinity("borrow_mut_gen_only");

    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    if generational_active {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", false);
        // 注意：这里缺少 mark_page_dirty_for_borrow() 调用！
    }

    self.inner.borrow_mut()
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `cell.rs:272-282`

**原因：**

1. `borrow_mut()` 在调用 `borrow_mut()` 之前**始终**调用 `mark_page_dirty_for_borrow()` (bug583 修复)
2. `borrow_mut_gen_only()` 调用 `gc_cell_validate_and_barrier()` 但**缺少** `mark_page_dirty_for_borrow()` 调用
3. `gc_cell_validate_and_barrier()` 对于年轻页面 (generation=0, gen_old=false) 会提前返回，不添加页面到 dirty_pages
4. 当使用 `GcCell<Vec<Gc<T>>>` 等类型时，children 不会被 trace，导致 premature collection

**影响：**

- 使用 `borrow_mut_gen_only()` 修改包含 `Gc<T>` 的类型时，children 可能被错误回收
- 这与 `borrow_mut()` 的行为不一致

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
#![cfg(feature = "test-util")]

use rudo_gc::{Gc, GcCell, Trace, collect_full, collect};
use rudo_gc::test_util;
use std::cell::RefCell;

#[derive(Clone, Trace)]
struct Child {
    value: i32,
}

#[derive(Clone, Trace)]
struct Parent {
    children: GcCell<Vec<Gc<Child>>>,
}

#[test]
fn test_borrow_mut_gen_only_missing_dirty_page() {
    test_util::reset();
    
    // Create parent with child
    let parent = Gc::new(Parent {
        children: GcCell::new(Vec::new()),
    });
    let child = Gc::new(Child { value: 42 });
    
    // Add child via borrow_mut_gen_only
    // This should mark the page dirty so child is traced during minor GC
    parent.children.borrow_mut_gen_only().push(child.clone());
    
    // Minor GC - child should survive if parent is root
    collect();
    
    // Access child - should still be valid
    let mut children = parent.children.borrow_mut();
    assert_eq!(children[0].value, 42);
}
```

**預期：** 測試通過
**實際：** 可能出現 "slot has been swept and reused" 錯誤

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `borrow_mut_gen_only()` 中添加 `mark_page_dirty_for_borrow()` 调用：

```rust
pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T> {
    self.validate_thread_affinity("borrow_mut_gen_only");

    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    if generational_active {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", false);
    }

    // FIX bug630: Always mark page dirty when borrow_mut_gen_only is called.
    // This ensures children in GcCell<Vec<Gc<T>>> are traced during minor GC.
    // The gen_old optimization in gc_cell_validate_and_barrier handles whether
    // to record the OLD→YOUNG reference, but we must always mark dirty so the
    // page is scanned.
    unsafe {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        crate::heap::mark_page_dirty_for_borrow(ptr);
    }

    self.inner.borrow_mut()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- `borrow_mut_gen_only()` 的設計目的是提供「僅世代屏障」以提高效能
- 但即使跳過 incremental barrier，generational barrier 仍然需要確保 page 被加入 dirty_pages
- 否則 minor GC 時 children 無法被追蹤

**Rustacean (Soundness 觀點):**
- 這是記憶體安全問題 - 使用 `borrow_mut_gen_only()` 可能導致 UAF
- 修復很簡單：始終調用 `mark_page_dirty_for_borrow()`
- 與 `borrow_mut()` 行為保持一致

**Geohot (Exploit 觀點):**
- 在並髮環境中，如果攻擊者能控制 GC 時機，可能利用這個空窗
- 但由于這是本地記憶體問題，可利用性有限

---

## 📎 Related Issues
- bug312: GcCell::borrow_mut_gen_only 缺少世代寫屏障 (已修復generational barrier調用)
- bug314: GcThreadSafeCell::borrow_mut_gen_only 缺少世代寫屏障 (已修復)
- bug583: GcCell::borrow_mut() 缺少 mark_page_dirty_for_borrow (已修復)
- bug630: (本issue) - borrow_mut_gen_only 缺少 mark_page_dirty_for_borrow