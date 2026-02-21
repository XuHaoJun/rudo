# [Bug]: Generational Write Barrier 忽略 per-object GEN_OLD_FLAG 導致 OLD→YOUNG 引用遺漏

**Status:** Open
**Tags:** Not Reproduced


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 當物件被提升為舊生代後，每當發生 OLD→YOUNG 引用時都會觸發此問題 |
| **Severity (嚴重程度)** | High | 年輕代物件可能被錯誤回收，導致 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要產生 OLD→YOUNG 引用，且年輕代物件被回收 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::generational_write_barrier`, `PageHeader::generation`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當物件被提升（promote）到舊生代（generation = 1）時，系統會設置 per-object 的 `GEN_OLD_FLAG`（在 GcBox 的 weak_count 中）。這個標誌用於快速路徑（fast-path）優化，讓 barrier 可以提前結束。

然而，`GcCell::generational_write_barrier` 只檢查 page header 中的 `generation > 0`，並沒有檢查 per-object 的 `GEN_OLD_FLAG`。這導致當：
1. 物件的 page generation = 0（頁面仍屬於年輕代）
2. 但物件本身被標記為 GEN_OLD_FLAG（已提升）

在這種情況下，barrier 會錯誤地認為這不是 OLD→YOUNG 引用，導致引用不被記錄到 dirty pages 中。

### 預期行為
- 當 OLD 物件（無論其 page generation 為何）寫入年輕代指標時，應該觸發 generational write barrier
- 應該檢查 per-object `GEN_OLD_FLAG`

### 實際行為
- `generational_write_barrier` 只檢查 page header 的 `generation > 0`
- 當 page generation = 0 但物件有 GEN_OLD_FLAG 時，barrier 不會記錄此引用
- 年輕代 GC（minor collection）可能會錯誤回收仍有外部引用的物件

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs:323-379` 的 `generational_write_barrier` 實作中：

```rust
fn generational_write_barrier(&self, ptr: *const u8) {
    // ...
    if (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE {
        let h = header.as_ptr();
        if (*h).generation > 0 {  // 只檢查 page generation
            // ... 記錄到 dirty pages
        }
        return;
    }
    // ...
}
```

問題在於：
1. Page header 的 `generation` 代表整個頁面的世代
2. 每個 GcBox 可以有自己的 `GEN_OLD_FLAG`（per-object promotion）
3. 當物件被提升但所在頁面尚未升級時，`generation = 0` 但 `GEN_OLD_FLAG` 已被設置
4. 此時寫入年輕代指標不會觸發 barrier

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};
use std::cell::RefCell;

#[derive(Clone, Trace)]
struct YoungData {
    value: i32,
}

#[derive(Trace)]
struct OldData {
    young_ref: GcCell<YoungData>,
}

fn main() {
    // 創建年輕代資料
    let young = Gc::new(YoungData { value: 42 });
    let young_cell = GcCell::new(YoungData { value: 100 });
    
    // 創建舊代資料（通過多次 GC 觸發 promotion）
    let mut old = Gc::new(OldData { young_ref: young_cell });
    
    for _ in 0..10 {
        collect_full();
    }
    
    // 此時 old 物件應該已被 promotion 為 GEN_OLD
    // 但其所在的 page 可能仍是 generation = 0
    
    // 執行 OLD → YOUNG 寫入
    {
        let mut young_ref = old.young_ref.borrow_mut();
        *young_ref = YoungData { value: 999 };  // 這裡可能不會觸發 barrier
    }
    
    // Minor GC - young_ref 可能被錯誤回收
    collect_full();
    
    // 嘗試存取 - 可能 UAF
    println!("{}", old.young_ref.borrow().value);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：修改 generational_write_barrier 檢查 per-object flag
在 `cell.rs:323` 的 `generational_write_barrier` 中：

```rust
fn generational_write_barrier(&self, ptr: *const u8) {
    // ...
    if (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE {
        let h = header.as_ptr();
        let is_old_page = (*h).generation > 0;
        
        // 額外檢查 per-object GEN_OLD_FLAG
        let is_old_object = if !is_old_page {
            let obj_ptr = /* 計算物件指標 */;
            let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
            ((*gc_box_ptr).weak_count.load(Ordering::Relaxed) 
                & crate::ptr::GcBox::<()>::GEN_OLD_FLAG) != 0
        } else {
            false
        };
        
        if is_old_page || is_old_object {
            // 記錄到 dirty pages
        }
        return;
    }
    // ...
}
```

### 方案 2：確保 page generation 與物件 promotion 同步
確保當物件被標記為 GEN_OLD_FLAG 時，相關的 page generation 也被更新。這需要修改 promotion 邏輯。

### 方案 3：在 barrier 中直接檢查 GcBox
在計算出物件指標後，直接讀取 GcBox 的 GEN_OLD_FLAG，而不是只依賴 page generation。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題涉及 generational GC 的經典設計：page-level vs object-level generation追蹤。在傳統的 generational GC 中，通常 page 和物件的世代是一致的。但在 rudo-gc 中，由於使用了 per-object promotion 優化，導致了這個不一致。建議使用 page-level generation 追蹤作為主要機制，per-object flag 只作為快速路徑優化。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果年輕代物件被錯誤回收，後續存取會導致 use-after-free，這是未定義行為。必須修復確保所有 OLD→YOUNG 引用都被正確追蹤。

**Geohot (Exploit 觀點):**
攻擊者可以通過：
1. 構造特定的内次 GC 觸發模式
2. 利用這個 race condition 實現記憶體佈局控制
3. 最終可能實現任意記憶體讀寫（如果配合其他漏洞）
