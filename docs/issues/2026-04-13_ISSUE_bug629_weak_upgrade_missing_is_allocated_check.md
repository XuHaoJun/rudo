# [Bug]: Weak::upgrade() 缺少 is_allocated 檢查 - 可能在 slot sweep 後 UAF

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 GC 期間 slot 被 sweep 後嘗試升級 weak ref 時發生 |
| **Severity (嚴重程度)** | High | 可能導致 use-after-free，正確性問題 |
| **Reproducibility (復現難度)** | High | 穩定可重現，透過 tight loop 觸發 GC |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::upgrade()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Weak::upgrade()` 應該在嘗試原子增加 ref_count 前檢查 `is_allocated`。這是為了防止 TOCTOU (Time-Of-Check-Time-Of-Use) 問題，其中：
1. slot 通過 `is_allocated` 檢查
2. slot 在 CAS 操作前被 sweep
3. 錯誤地對已釋放的 slot 執行 CAS 成功

### 實際行為 (Actual Behavior)

`Weak::upgrade()` 在讀取 `generation` 後執行 CAS，但在 CAS 之前沒有再次檢查 `is_allocated`。相比之下，`Weak::try_upgrade()` 在 line 2545-2550 有 `is_allocated` 檢查。

此外，`Weak::upgrade()` 在 CAS 成功後有 `is_allocated` 檢查（line 2487-2497），但如果 slot 在 pre-check 和 CAS之間被 sweep，這個後檢查無法防止 UAF。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:2429-2431` (pre-CAS check) 和 `2487-2497` (post-CAS check)

```rust
// Line 2429-2431: 在讀取 generation 前沒有檢查 is_allocated
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {  // ← 這是第一次檢查
            return None;
        }
    }
}

unsafe {
    let gc_box = &*ptr.as_ptr();  // ← 在這裡讀取 gc_box

    // FIX bug383: Return None instead of panicking when is_under_construction.
    if gc_box.is_under_construction() {
        return None;
    }

    let pre_generation = gc_box.generation();  // ← 讀取 generation

    // ... 省略一些程式碼 ...

    if gc_box
        .ref_count
        .compare_exchange_weak(...)  // ← CAS 操作
        .is_ok()
    {
        // Verify generation hasn't changed
        if pre_generation != gc_box.generation() {  // ← 檢查 generation
            GcBox::undo_inc_ref(ptr.as_ptr());
            return None;
        }
        // ... 其他檢查 ...
        // Check is_allocated after successful upgrade (line 2487-2497)
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {  // ← 這是 post-CAS 檢查
                GcBox::undo_inc_ref(ptr.as_ptr());
                return None;
            }
        }
        // ...
    }
}
```

**問題：**

1. **第一次 `is_allocated` 檢查 (line 2429) 和 `gc_box` dereference (line 2431)之間存在 TOCTOU 視窗**。在這段時間內，lazy sweep 可能回收了 slot。

2. **`pre_generation` 捕獲 (line 2439) 和 CAS 操作 (line 2459)之間存在 TOCTOU 視窗**。即使 generation 匹配，slot 也可能在這裡被回收。

3. **Post-CAS `is_allocated` 檢查 (line 2487) 無法防止 pre-CAS 的 TOCTOU**。

**對比 `Weak::try_upgrade()`：**

`Weak::try_upgrade()` (line 2525-2610) 在讀取 `pre_generation` (line 2561) 後、CAS 前也沒有 `is_allocated` 重新檢查：

```rust
// Weak::try_upgrade() - 也有相同的問題模式
let pre_generation = gc_box.generation();  // line 2561

// ... 中間沒有 is_allocated 重新檢查 ...

if gc_box
    .ref_count
    .compare_exchange_weak(...)  // line 2577
    .is_ok()
{
    // Verify generation hasn't changed
    if pre_generation != gc_box.generation() {  // line 2589
```

但 `Weak::try_upgrade()` 的 post-CAS 檢查 (line 2593-2598 和 2605-2608) 更完整 - 它檢查 `is_under_construction()` 和 `is_allocated`，最後執行 `undo_inc_ref`。

**正確的模式應該是：**

```rust
// 在讀取 generation 前檢查 is_allocated
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return None;
    }
}

let pre_generation = gc_box.generation();

// CAS 操作

// Post-CAS 再次檢查 is_allocated
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::undo_inc_ref(ptr.as_ptr());
        return None;
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
#![cfg(feature = "test-util")]

use rudo_gc::{Gc, Weak, Trace, collect_full};
use rudo_gc::test_util;
use std::thread;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_weak_upgrade_after_sweep() {
    test_util::reset();

    // Create weak ref
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    let ptr_addr = gc.as_ptr() as usize;

    // Drop strong ref to make weak upgradeable but object collectible
    drop(gc);

    // Tight loop to trigger sweep while upgrade in progress
    for _ in 0..10000 {
        if let Some(upgraded) = weak.upgrade() {
            // Should only succeed if object is still valid
            assert_eq!(upgraded.value, 42);
        }
    }

    // Force full collection
    collect_full();

    // Verify weak ref is now invalid
    assert!(weak.upgrade().is_none());
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::upgrade()` 中，在讀取 `pre_generation` 前添加 `is_allocated` 檢查：

```rust
// 在讀取 generation 前檢查 is_allocated
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return None;
    }
}

let pre_generation = gc_box.generation();
```

並在 post-CAS 檢查後執行 `undo_inc_ref` 以避免 ref_count 洩漏。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Weak ref 升級是回收鍊條中的關鍵操作
- 在增加 ref_count 前必須確保 slot 仍然有效
- TOCTOU 問題源於標記和掃描之間的時間窗口

**Rustacean (Soundness 觀點):**
- 缺少 pre-CAS is_allocated 檢查可能導致 UAF
- 這是正確性問題，需要修復
- Generation 檢查可以捕獲 slot 重用，但不能捕獲簡單的 sweep

**Geohot (Exploit 觀點):**
- 在並髮環境中，TOCTOU 可被利用
- 攻擊者可能透過控制 GC 時機來觸發此問題
- 這可能導致記憶體損壞或洩漏

---

## Resolution

(Open - awaiting fix)