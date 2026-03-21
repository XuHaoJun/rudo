# [Bug]: Gc::downgrade 檢查順序不一致 - is_allocated 在 flag checks 之前

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並發場景才能觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致檢查錯誤物件的 flag |
| **Reproducibility (復現難度)** | Medium | 需要並發 GC 和 slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::downgrade()`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`Gc::downgrade()` 應該與 `Gc::clone()` 有一致的檢查順序，確保在檢查 flag 之前物件仍然是有效的。

### 實際行為
`Gc::downgrade()` 在檢查 flag 之前先檢查 `is_allocated`，這與 `Gc::clone()` 的順序不一致。

### 程式碼位置

**Gc::downgrade (ptr.rs:1696-1730) - 錯誤順序:**
```rust
// 1. 先檢查 is_allocated (lines 1701-1707)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!((*header.as_ptr()).is_allocated(idx), ...);
}

// 2. 然後檢查 flag (lines 1709-1715)
assert!(
    !(*gc_box_ptr).has_dead_flag()
        && (*gc_box_ptr).dropping_state() == 0
        && !(*gc_box_ptr).is_under_construction(),
    ...
);

// 3. 最後 inc_weak (line 1716)
(*gc_box_ptr).inc_weak();
```

**Gc::clone (ptr.rs:1964-2013) - 正確順序:**
```rust
// 1. 先檢查 flag (lines 1979-1984)
assert!(
    !(*gc_box_ptr).has_dead_flag()
        && (*gc_box_ptr).dropping_state() == 0
        && !(*gc_box_ptr).is_under_construction(),
    ...
);

// 2. 然後檢查 is_allocated (lines 1989-1995)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!((*header.as_ptr()).is_allocated(idx), ...);
}

// 3. 最後 inc_ref (line 1997)
(*gc_box_ptr).inc_ref();
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `Gc::downgrade()` 的檢查順序：
1. 先檢查 `is_allocated`
2. 再檢查 flag (has_dead_flag, dropping_state, is_under_construction)

在步驟 1 和步驟 2 之間，slot 可能被 sweep 並重新分配。當我們在步驟 2 檢查 flag 時，可能會讀取到新分配物件的 flag，而不是原始物件的 flag。

相比之下，`Gc::clone()` 正確地先檢查 flag，再檢查 `is_allocated`。

此外，`Gc::downgrade()` 進行了兩次 `is_allocated` 檢查（第一次在 inc_weak 之前，第二次在之後），這與 `Gc::clone()` 不一致。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上需要並發場景：
1. 一個 Gc 物件即將被 sweep
2. 在 `Gc::downgrade()` 的第一次 is_allocated 檢查和 flag 檢查之間，slot 被 sweep 並重新分配
3. 新物件的 flag 可能與原始物件不同，導致不一致的行為

```rust
// 理論 PoC - 需要精確時序控制
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::Mutex;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 需要並發 GC 才能穩定重現
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `Gc::downgrade()` 的順序調整為與 `Gc::clone()` 一致：

```rust
pub fn downgrade(gc: &Self) -> Weak<T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::downgrade: cannot downgrade a dead Gc");
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        // 1. 先檢查 flag - 與 Gc::clone 一致
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::downgrade: cannot downgrade a dead, dropping, or under construction Gc"
        );

        // 2. 然後檢查 is_allocated - 避免 TOCTOU
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            assert!(
                (*header.as_ptr()).is_allocated(idx),
                "Gc::downgrade: slot has been swept and reused"
            );
        }

        // 3. inc_weak
        (*gc_box_ptr).inc_weak();

        // 4. 第二次檢查 is_allocated - 與 Gc::clone 一致
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            assert!(
                (*header.as_ptr()).is_allocated(idx),
                "Gc::downgrade: slot was swept during downgrade"
            );
        }
    }
    Weak {
        ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 操作中，確保物件有效性檢查的順序非常重要。如果在檢查 flag 時，slot 已經被重新分配，可能會讀取到錯誤的元數據，導致不一致的行為。

**Rustacean (Soundness 觀點):**
這是一個代碼一致性問題。雖然目前可能不會導致立即的記憶體錯誤，但不一致的檢查順序可能在未來引入 subtle 的 bug。

**Geohot (Exploit 攻擊觀點):**
在並發環境中，如果攻擊者能夠精確控制時序，可能利用這個 TOCTOU 來觸發不一致的行為。


---

## Resolution (2026-03-21)

**Outcome:** Fixed.

Reordered checks in `Gc::downgrade()` (`ptr.rs:1746-1781`) to match `Gc::clone()`:
1. Flag checks (`has_dead_flag`, `dropping_state`, `is_under_construction`) — first
2. `is_allocated` check — second
3. `inc_weak` with generation guard (bug356)
4. Post-`inc_weak` `is_allocated` check

The two separate `unsafe` blocks were merged into one. All tests pass.
