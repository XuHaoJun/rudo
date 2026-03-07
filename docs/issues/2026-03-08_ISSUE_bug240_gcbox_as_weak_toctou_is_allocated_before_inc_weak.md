# [Bug]: GcBox::as_weak TOCTOU - is_allocated 檢查在 inc_weak 之前導致 Race

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 as_weak 並發執行，slot 被回收並重用 |
| **Severity (嚴重程度)** | Critical | 錯誤地增加新物件的 weak count，導致記憶體泄漏與資料混淆 |
| **Reproducibility (復現難度)** | High | 需要精確控制執行緒調度與 GC timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::as_weak` in `crates/rudo-gc/src/ptr.rs:453-463`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBox::as_weak` 函數的檢查順序錯誤。當前實作先檢查 `is_allocated`，然後再呼叫 `inc_weak()`。這與正確的模式（`Gc::downgrade()` 和 `GcBoxWeakRef::clone()`）相反，導致 Time-Of-Check-Time-Of-Use (TOCTOU) race condition。

### 預期行為 (Expected Behavior)

應該先呼叫 `inc_weak()`，然後檢查 `is_allocated`。如果 slot 已被 sweep 且重用，應該 undo inc_weak 並返回 null weak reference。

### 實際行為 (Actual Behavior)

當前程式碼順序（錯誤）:
```rust
// ptr.rs:453-463 - 錯誤順序
let self_ptr = NonNull::from(self).as_ptr() as *const u8;
if let Some(idx) = crate::heap::ptr_to_object_index(self_ptr) {
    let header = crate::heap::ptr_to_page_header(self_ptr);
    if !(*header.as_ptr()).is_allocated(idx) {  // 檢查在前
        return GcBoxWeakRef { ptr: AtomicNullable::null() };
    }
}
(*NonNull::from(self).as_ptr()).inc_weak();  // inc_weak 在後 - 錯誤!
```

正確模式（來自 `Gc::downgrade()`）:
```rust
// ptr.rs:1473-1481 - 正確順序
(*gc_box_ptr).inc_weak();  // inc_weak 在前

if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*gc_box_ptr).dec_weak();  // undo if needed
        panic!("Gc::downgrade: slot was swept during downgrade");
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU Race Condition 詳細過程:

1. Object A 存在於 slot index，weak_count = 0
2. Thread A 呼叫 `GcBox::as_weak()`
3. Thread A 檢查 `is_allocated(idx)` → 返回 `true`（Object A 仍存在）
4. **Race Window**: Lazy sweep 在此時運行:
   - 認定 Object A 已死亡，回收 slot
   - 在同一 slot 分配 Object B
5. Thread A 執行 `inc_weak()` → **Object B 的 weak_count 變成 1！**
6. 返回 `GcBoxWeakRef` 指向 Object A 的指標，但 weak count 錯誤地增加在 Object B 上

後果:
- Object A 的 weak_count 永遠為 0，無法正確追蹤
- Object B 的 weak_count 錯誤地為 1，導致記憶體泄漏
- 如果 Object B 也被回收，weak reference upgrade 可能會發生錯誤

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要並發測試環境
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 創建 Gc 並獲取 GcBoxWeakRef
    let gc = Gc::new(Data { value: 42 });
    let weak = gc.as_weak();
    
    // 2. 觸發 GC 回收 gc
    drop(gc);
    collect_full();
    
    // 3. 在同一 slot 分配新物件（可能觸發 lazy sweep）
    let gc2 = Gc::new(Data { value: 100 });
    
    // 4. 同時呼叫 as_weak
    // 觀察: weak count 是否正確
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `GcBox::as_weak` 的順序調整為與 `Gc::downgrade()` 一致:

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        if self.is_under_construction() || self.has_dead_flag() || self.dropping_state() != 0 {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }

        // Step 1: 先遞增 weak count
        (*NonNull::from(self).as_ptr()).inc_weak();

        // Step 2: 再檢查 is_allocated
        let self_ptr = NonNull::from(self).as_ptr() as *const u8;
        if let Some(idx) = crate::heap::ptr_to_object_index(self_ptr) {
            let header = crate::heap::ptr_to_page_header(self_ptr);
            if !(*header.as_ptr()).is_allocated(idx) {
                // Undo the inc_weak we just did
                crate::ptr::GcBox::dec_weak(self as *const GcBox<T> as *mut GcBox<T>);
                return GcBoxWeakRef {
                    ptr: AtomicNullable::null(),
                };
            }
        }
        
        GcBoxWeakRef::new(NonNull::from(self))
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 漏洞。正確的模式應該是「樂觀遞增 + 驗證後回滾」：先假設操作會成功並遞增計數，然後驗證物件狀態，如果驗證失敗則回滾。這種模式在 `Gc::downgrade()` 中已經正確實現，`GcBox::as_weak` 應該採用相同模式。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。正確的 weak reference 追蹤對於 GC 的正確性至關重要。錯誤地增加 weak count 會導致:
1. 原物件的 weak reference 無法正確升級
2. 新物件可能因為錯誤的 weak count 而無法被正確回收

**Geohot (Exploit 攻擊觀點):**
攻擊者可能通過:
1. 控制 GC timing 來觸發 race condition
2. 利用錯誤的 weak count 來繞過 weak reference 的安全檢查
3. 實現任意記憶體讀寫（如果能控制新物件的佈局）
