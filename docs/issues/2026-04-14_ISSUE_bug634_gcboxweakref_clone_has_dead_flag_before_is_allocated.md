# [Bug]: GcBoxWeakRef::clone reads has_dead_flag/dropping_state before is_allocated check (TOCTOU)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 lazy sweep 期間 slot 被 sweep 後嘗試 clone weak ref 時發生 |
| **Severity (嚴重程度)** | Medium | 可能讀取已釋放記憶體的狀態，但 is_allocated 檢查提供最終保護 |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::clone()` (ptr.rs:820-890)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

根據 bug580 建立的模式，`is_allocated` 檢查應該在讀取任何 GcBox 欄位之前完成：

```
is_allocated -> has_dead_flag/dropping_state/is_under_construction -> generation -> inc_weak
```

### 實際行為 (Actual Behavior)

`GcBoxWeakRef::clone()` 在 line 836 dereference 指針後，於 line 841-847 讀取 `has_dead_flag()` 和 `dropping_state()`，然後才在 line 852-857 做 `is_allocated` 檢查：

```rust
// Line 836: DEREFERENCE
let gc_box = &*ptr.as_ptr();

// Lines 841-847: 讀取 has_dead_flag 和 dropping_state - 此時 slot 可能已被 sweep！
if gc_box.has_dead_flag() {  // BUG: 在 is_allocated 之前讀取
    return Self::null();
}
if gc_box.dropping_state() != 0 {  // BUG: 在 is_allocated 之前讀取
    return Self::null();
}

// Lines 852-857: is_allocated 檢查在這些讀取之後
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {  // 這裡才檢查 slot 是否仍分配
        return Self::null();
    }
}
```

### 對比 Weak::clone() 的正確模式

`Weak::clone()` (lines 2840-2889) 同樣在 line 2841 dereference 後讀取 `has_dead_flag()` 和 `dropping_state()`，然後才在 line 2858-2865 做 `is_allocated` 檢查 - 相同的問題模式！

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:836-857`

**TOCTOU 視窗：**
1. Line 831: `is_gc_box_pointer_valid()` 通過（指標在 heap 範圍內）
2. Line 836: dereference 指針
3. Lines 841-847: 讀取 `has_dead_flag()` 和 `dropping_state()` - **此時 slot 可能已被 lazy sweep 回收！**
4. Lines 852-857: `is_allocated` 檢查 - 這裡才確認 slot 是否仍分配

**問題：**
- 如果 slot 在 `is_gc_box_pointer_valid()` 和 `is_allocated` 檢查之間被 sweep：
  - `has_dead_flag()` 和 `dropping_state()` 從已釋放記憶體讀取
  - 雖然 `is_allocated` 最終會返回 false，但我們已經從已釋放記憶體讀取了狀態

**現有保護：**
- Generation 檢查 (lines 865-868) 在 slot 被重用時會 catch
- 第二次 `is_allocated` 檢查 (lines 873-884) 提供 defense-in-depth

**為什麼這仍然是個問題：**
1. 讀取已釋放記憶體在 Rust 中是 UB
2. 不符合 bug580 建立的模式
3. 如果記憶體被錯誤地重複使用且 generation 未變化，會造成錯誤的操作

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
fn test_gcboxweakref_clone_has_dead_flag_toctou() {
    test_util::reset();
    
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    drop(gc);
    
    for _ in 0..10000 {
        let _ = weak.clone();
    }
    
    collect_full();
}
```

**注意：** 此 bug 需要精確的執行緒時序才能穩定重現。單執行緒測試可能無法可靠觸發。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

移動 `is_allocated` 檢查到 `has_dead_flag()` 和 `dropping_state()` 讀取之前：

```rust
pub(crate) fn clone(&self) -> Self {
    // ... existing pointer validation ...
    
    if !is_gc_box_pointer_valid(ptr_addr) {
        return Self::null();
    }

    unsafe {
        // FIX bugXXX: Check is_allocated BEFORE reading has_dead_flag/dropping_state.
        // This matches the pattern from bug580: is_allocated -> flag checks -> generation -> inc_weak.
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                return Self::null();
            }
        }
        
        // NOW safe to dereference and read flags
        let gc_box = &*ptr.as_ptr();
        
        // Note: We do NOT check is_under_construction here. Gc::new_cyclic_weak
        // passes a Weak to the closure while the object is under construction;
        // the closure may legitimately clone it.
        if gc_box.has_dead_flag() {
            return Self::null();
        }
        
        if gc_box.dropping_state() != 0 {
            return Self::null();
        }
        
        // ... rest of function ...
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 標準 GC 安全原則：驗證 slot 有效性後才能讀取物件狀態
- `is_gc_box_pointer_valid` 只驗證指標格式，不驗證分配狀態
- 完整的 TOCTOU 防護需要：`is_allocated` 檢查在讀取任何 GcBox 欄位之前

**Rustacean (Soundness 觀點):**
- 讀取已釋放記憶體是 UB，即使後續有檢查
- 當前程式碼與 bug580 建立的模式不一致
- `Weak::clone()` 有相同的問題模式

**Geohot (Exploit 觀點):**
- 如果攻擊者能控制 GC 時機，可能穩定觸發此 TOCTOU 視窗
- 讀取已釋放記憶體可能導致資訊洩漏（從新物件讀取錯誤狀態）

---

## 相關 Bug

- bug564: GcBoxWeakRef::clone reads generation before is_allocated (fixed)
- bug580: GcHandle::downgrade check ordering (correct pattern established)
- bug600: GcBoxWeakRef::clone second is_allocated check (defense-in-depth)
- bug631: GcBoxWeakRef::upgrade missing pre-dereference is_allocated check