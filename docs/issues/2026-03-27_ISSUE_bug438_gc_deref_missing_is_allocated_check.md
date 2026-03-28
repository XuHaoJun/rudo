# [Bug]: Gc::deref 缺少 is_allocated 檢查導致 Slot Reuse 後存取錯誤物件

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Gc deref 併發執行，slot 被回收並重新配置 |
| **Severity (嚴重程度)** | Critical | 會存取到錯誤物件的資料，導致記憶體錯誤 |
| **Reproducibility (重現難度)** | High | 需要精確的時序控制來觸發 slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::deref()` (ptr.rs:2071-2084)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Gc::deref()` 應該在解引用前檢查 `is_allocated`，確保 slot 未被 sweep 並重新分配給新物件。

### 實際行為 (Actual Behavior)

`Gc::deref()` 目前沒有 `is_allocated` 檢查，只依靠 `has_dead_flag()`、`dropping_state()` 和 `is_under_construction()` 來驗證。

問題：當 slot 被 sweep 並重新分配給新物件時：
- 新物件的 `has_dead_flag() = false`
- 新物件的 `dropping_state() = 0`
- 新物件的 `is_under_construction() = false`

所有檢查都會通過，但實際上存取的是新物件的資料，而非原本的物件！

對比 `try_deref()` (ptr.rs:1556-1580) 已有 `is_allocated` 檢查：

```rust
// try_deref 有 is_allocated 檢查 (lines 1564-1569)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return None;
    }
}
```

但 `deref()` 沒有這個檢查：

```rust
// deref 沒有 is_allocated 檢查 (lines 2071-2084)
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::deref: cannot dereference a null Gc");
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::deref: cannot dereference a dead, dropping, or under construction Gc"
        );
        &(*gc_box_ptr).value  // BUG: 可能存取到新物件的資料！
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 slot reuse 與 deref 併發執行時：
1. 物件 A 在 slot X 被配置，使用者持有 `Gc<A>` 指針 P1
2. 物件 A 被 drop，ref_count 歸零，sweep 回收 slot X
3. 新物件 B 在同一 slot X 被配置（記憶體位址相同）
4. 使用者呼叫 `deref()` 取得 P1 指向的物件
5. **BUG:** 檢查都通過，但取得的是物件 B 的資料！

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct OldData { id: AtomicUsize }
#[derive(Trace)]  
struct NewData { value: i32 }

fn main() {
    // 1. 建立 Gc 物件 A (OldData)
    let gc = Gc::new(OldData { id: AtomicUsize::new(42) });
    let ptr = gc.as_ptr() as usize;
    
    // 2. 強制觸發 GC 來 drop 這個物件
    drop(gc);
    collect_full();
    
    // 3. 在同一 slot 配置新物件 B (NewData)
    let new_gc = Gc::new(NewData { value: 100 });
    
    // 4. 如果 slot 被重用，gc_old 的內部指標仍指向相同位址
    // 但 deref() 會錯誤地存取 NewData 的 vtable 而非 OldData
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `deref()` 中添加 `is_allocated` 檢查，與 `try_deref()` 保持一致：

```rust
#[inline]
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::deref: cannot dereference a null Gc");
    let gc_box_ptr = ptr.as_ptr();
    
    // FIX bug207: Add is_allocated check to prevent accessing wrong object after slot reuse
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            assert!(
                (*header.as_ptr()).is_allocated(idx),
                "Gc::deref: slot has been swept and reused"
            );
        }
    }
    
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::deref: cannot dereference a dead, dropping, or under construction Gc"
        );
        &(*gc_box_ptr).value
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Slot reuse 會導致舊指標指向新物件，這是經典的記憶體安全問題。`try_deref()` 已有 `is_allocated` 檢查，`deref()` 應該也要有。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。使用者期望存取物件 A，實際卻存取到物件 B，導致資料混淆或型別錯誤。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以嘗試控制 slot reuse 的時序，讓舊指標指向攻擊者控制的物件，實現任意記憶體讀寫。

---

## 相關 Issue

- bug207: Gc::deref 缺少 is_allocated 檢查 (本 issue)
- bug197: Gc 核心方法缺少 is_allocated 檢查
- try_deref(): 已有 is_allocated 檢查 (正確的實作)

---

## Resolution (2026-03-28)

**Outcome:** Fixed.

`Gc::deref()` in `ptr.rs` asserts `PageHeader::is_allocated` when `ptr_to_object_index` applies, matching `try_deref()` / `try_clone()`. The prior module comment that claimed `is_allocated` was skipped was updated to match the implementation.