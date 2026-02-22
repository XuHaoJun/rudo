# [Bug]: Write Barrier 僅檢查 per-object GEN_OLD_FLAG 忽略 Page Generation 導致 OLD→YOUNG 引用遺漏

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當物件在新分配的舊生代頁面中被 mutated 時會觸發 |
| **Severity (嚴重程度)** | High | 年輕代物件可能被錯誤回收，導致 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要minor GC觸發，且新舊生代引用關係 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `unified_write_barrier`, `gc_cell_validate_and_barrier` in `heap.rs`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

在 `heap.rs` 的 write barrier 實作中（如 `gc_cell_validate_and_barrier` 和 `unified_write_barrier`），barrier 只檢查 per-object 的 `GEN_OLD_FLAG`（透過 `has_gen_old_flag()`），但**沒有檢查 page 本身的 generation**。

當物件 newly allocated 在 OLD generation page（generation > 0）但尚未經歷過 GC 存活下來，該物件不會有 `GEN_OLD_FLAG`。此時對該物件進行寫入（例如寫入一個年輕代指標），barrier 會錯誤地跳過記錄 dirty page。

### 預期行為
- 當 OLD 頁面中的物件（無論是否有 GEN_OLD_FLAG）寫入年輕代指標時，應觸發 generational write barrier
- 應該檢查 page header 的 `generation > 0`

### 實際行為
- `gc_cell_validate_and_barrier` (line 2769) 和 `unified_write_barrier` (line 2839) 只檢查 `has_gen_old_flag()`
- 當物件沒有 GEN_OLD_FLAG（即使其 page generation > 0），barrier 不會記錄此引用
- 年輕代 GC（minor collection）可能會錯誤回收仍有外部引用的物件

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs:2766-2771` 的 `gc_cell_validate_and_barrier` 中：

```rust
// Line 2769 in heap.rs
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
if !(*gc_box_addr).has_gen_old_flag() {
    return;  // BUG: Skips barrier without checking page generation!
}
(*h.as_ptr()).set_dirty(index);
heap.add_to_dirty_pages(h);
```

問題在於：
1. `GEN_OLD_FLAG` 是 per-object flag，只有在物件於 GC 後存活下來才會設置（於 `promote_young_pages()` 中）
2. 新分配在 OLD page（generation > 0）的物件一開始沒有這個 flag
3. 當此物件被 mutate 引用到年輕代物件時，barrier 錯誤地跳過
4. 應該檢查 page header 的 `generation > 0` 而非僅依賴 per-object flag

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full, collect};
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
    
    // 先 full collect 確保 heap 乾淨
    collect_full();
    
    // 創建舊代資料（通過多次 GC 觸發 promotion）
    let old = Gc::new(OldData { young_ref: GcCell::new(YoungData { value: 0 }) });
    
    for _ in 0..10 {
        collect_full();
    }
    
    // 此時 old 物件的 page 應該是 = 1
    // 但如果 generation我們新建一個新的 GcCell 在同一個舊頁面中...
    
    // 在舊頁面中分配新的 GcCell (沒有 GEN_OLD_FLAG)
    let new_old_cell: GcCell<YoungData> = GcCell::new(YoungData { value: 200 });
    
    // 執行 OLD → YOUNG 寫入（透過 borrow_mut 觸發 barrier）
    {
        let mut cell_ref = new_old_cell.borrow_mut();
        *cell_ref = YoungData { value: 999 };
    }
    
    // Minor GC - new_old_cell 沒有被記錄到 dirty pages
    // 因為 barrier 檢查 has_gen_old_flag() 返回 false
    // 注意：這裡需要觸發 minor GC (collect() 而非 collect_full())
    collect();
    
    // 如果 bug 存在，young 物件可能被錯誤回收
    println!("Success!");
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案：修改 barrier 檢查 page generation

在 `gc_cell_validate_and_barrier` 和 `unified_write_barrier` 中，將檢查順序改為：
1. 先檢查 page header 的 `generation > 0`（OLD page）
2. 如果是 OLD page，則記錄 dirty
3. 如果不是 OLD page，才檢查 per-object `GEN_OLD_FLAG`（作為優化）

```rust
// 修改後的邏輯（概念）
let page_gen = (*h.as_ptr()).generation;
let has_old_flag = (*gc_box_addr).has_gen_old_flag();

if page_gen == 0 && !has_old_flag {
    return;  // Both young: skip barrier
}
// Either old page OR old object: record dirty
(*h.as_ptr()).set_dirty(index);
heap.add_to_dirty_pages(h);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題與 bug3 互補。Bug3 是關於 page 年輕但物件已 promotion 的情況（page gen=0 但有 GEN_OLD_FLAG）。本 bug 是相反：page 已 old 但物件尚未經歷過 GC（page gen>0 但無 GEN_OLD_FLAG）。傳統generational GC 通常使用 page-level 追蹤為主，per-object flag 僅作為優化捷徑。rudo-gc 目前的實作順序錯誤，應該先檢查 page level。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果年輕代物件被錯誤回收，後續存取會導致 use-after-free，這是未定義行為。Per-object flag 應該是優化而非主要機制。

**Geohot (Exploit 觀點):**
攻擊者可以通過：
1. 強制觸發舊生代頁面中的新物件分配
2. 利用這個 barrier 缺陷實現 young object 的提前回收
3. 配合其他漏洞可能實現記憶體操縱
