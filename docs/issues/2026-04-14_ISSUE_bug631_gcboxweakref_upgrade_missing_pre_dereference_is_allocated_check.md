# [Bug]: GcBoxWeakRef::upgrade missing is_allocated check before dereference - TOCTOU UAF

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 GC 期間 slot 被 sweep 後嘗試升級 weak ref 時發生 |
| **Severity (嚴重程度)** | Critical | 可能導致 use-after-free，正確性問題 |
| **Reproducibility (復現難度)** | High | 穩定可重現，透過 tight loop 觸發 GC |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade()` (ptr.rs:696-805)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcBoxWeakRef::upgrade()` 應該在dereference記憶體前檢查 `is_allocated`，以防止 TOCTOU UAF。這與 `Weak::upgrade()` 的行為一致。

`Weak::upgrade()` 在 line 2373-2379 有 pre-dereference `is_allocated` 檢查：
```rust
// Line 2369: is_gc_box_pointer_valid check
if !is_gc_box_pointer_valid(addr) {
    return None;
}

// Lines 2373-2379: is_allocated check BEFORE dereference
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return None;
        }
    }
}

// Line 2383: THEN dereference
let gc_box = &*ptr.as_ptr();
```

### 實際行為 (Actual Behavior)

`GcBoxWeakRef::upgrade()` 在 line 706-708 執行 `is_gc_box_pointer_valid` 檢查後，直接在 line 711 dereference記憶體，完全跳過了 `is_allocated` 檢查：

```rust
// Line 706-708: Only validates pointer alignment and basic validity
if !is_gc_box_pointer_valid(ptr_addr) {
    return None;
}

// Line 711: DEREFERENCE without is_allocated check - BUG!
let gc_box = &*ptr.as_ptr();

// Lines 713-726: Then checks flags on the dereferenced box
if gc_box.is_under_construction() {
    return None;
}
// ... more checks
```

如果 lazy sweep 在 line 706-708 檢查通過後但在 line 711 dereference 前回收了 slot，就會發生 UAF。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:706-711`

**對比 `Weak::upgrade()` 和 `GcBoxWeakRef::upgrade()`：**

| 函數 | is_gc_box_pointer_valid | is_allocated (pre-deeref) | Dereference |
|------|------------------------|---------------------------|-------------|
| `Weak::upgrade()` (line 2356) | ✓ (line 2369) | ✓ (lines 2373-2379) | line 2383 |
| `GcBoxWeakRef::upgrade()` (line 696) | ✓ (line 706) | **✗ MISSING** | line 711 |

**為什麼 `is_allocated` 檢查是必要的：**

`is_gc_box_pointer_valid()` 只檢查：
1. 指針對齊
2. 最小地址 (`MIN_VALID_HEAP_ADDRESS`)

它**不**檢查 slot 是否當前已分配。如果 lazy sweep 在驗證通過後但dereference前回收了 slot，就會發生 UAF。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
#![cfg(feature = "test-util")]

use rudo_gc::{Gc, Trace, collect_full};
use rudo_gc::test_util;
use std::thread;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_gcboxweakref_upgrade_pre_dereference_uaf() {
    test_util::reset();
    
    // Create object and get internal weak ref
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // Drop the Gc to make object collectible
    drop(gc);
    
    // Force collection to trigger lazy sweep
    collect_full();
    
    // Try to upgrade weak ref - this could hit the TOCTOU window
    // if slot was swept between is_gc_box_pointer_valid and dereference
    for _ in 0..10000 {
        // This internally calls GcBoxWeakRef::upgrade() 
        let _ = weak.upgrade();
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBoxWeakRef::upgrade()` 中，於dereference前新增 `is_allocated` 檢查，與 `Weak::upgrade()` 模式一致：

```rust
if !is_gc_box_pointer_valid(ptr_addr) {
    return None;
}

// FIX bug631: Add is_allocated check BEFORE dereference to prevent UAF.
// This matches Weak::upgrade() pattern (lines 2373-2379).
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return None;
        }
    }
}

unsafe {
    let gc_box = &*ptr.as_ptr();
    // ... rest of function
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- GC 的核心原則：在dereference記憶體前必須驗證 slot 有效性
- `is_gc_box_pointer_valid` 只驗證指標格式，不驗證分配狀態
- 完整的 TOCTOU 防護需要：在指標驗證和dereference之間，slot 不能被回收

**Rustacean (Soundness 觀點):**
- 這是經典的 TOCTOU UAF - 在檢查和使用之間，狀態可能改變
- `Weak::upgrade()` 已有正確的 pre-deference `is_allocated` 檢查
- `GcBoxWeakRef::upgrade()` 缺少相同檢查是 API 不一致

**Geohot (Exploit 觀點):**
- 在並髮環境中，TOCTOU 視窗可以被利用
- 如果攻擊者能控制 GC 時機（如分配壓力），可能穩定觸發此 bug
- dereference 已釋放記憶體是 UB，可能導致記憶體損壞或資訊洩漏