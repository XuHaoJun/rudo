# [Bug]: GcCell 寫屏障在大型物件跨頁時失效導致 UAF

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 大型物件（>1 page）較少見，但當使用時會觸發此問題 |
| **Severity (嚴重程度)** | Critical | 寫屏障失效導致 SATB 擔保被破壞，可能造成 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要分配超過一頁的物件，並在第二頁放置 GcCell |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr_to_page_header`, `GcCell`, 寫屏障機制
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)
當 `GcCell<T>` 位於大型物件的第二頁（或後續頁面）時，寫屏障（write barrier）無法正確觸發。這是因為 `ptr_to_page_header()` 函數會將指標遮罩到頁面邊界，但大型物件只有第一頁有 `PageHeader`。對於 tail pages，此函數返回垃圾數據，magic 檢查失敗，寫屏障被跳過。

### 預期行為
當修改 `GcCell` 中的 GC 指標時（例如 `*gc.cell.borrow_mut() = young_obj`），無論 `GcCell` 位於哪一頁，寫屏障都應該正確記錄此修改，確保 SATB 擔保成立。

### 實際行為
當 `GcCell` 位於大型物件的第二頁（或後續頁面）時：
1. `ptr_to_page_header()` 返回錯誤的 header（tail page 沒有 PageHeader）
2. Magic 檢查失敗（`(*header.as_ptr()).magic != MAGIC_GC_PAGE`）
3. 寫屏障被提前返回（early return），不執行任何記錄
4. 年輕代物件被錯誤地回收，導致 use-after-free

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs` 的 `ptr_to_page_header()` 函數中：
```rust
pub const fn ptr_to_page_header(ptr: *const u8) -> NonNull<PageHeader> {
    let ptr_addr = ptr as usize;
    let page_mask = !(page_size() - 1);
    let page_addr = ptr_addr & page_mask;  // 遮罩到頁面邊界
    // ...
}
```

此函數將指標遮罩到最近的頁面邊界。對於大型物件：
- 只有第一頁有 `PageHeader` 和正確的 magic number
- 第二頁及後續頁面沒有 `PageHeader`，其記憶體內容被視為 header
- Magic 檢查隨機通過或失敗，導致不確定的行為

在 `unified_write_barrier()` 中：
```rust
if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
    return;  // 提前返回，跳過寫屏障
}
```

當 magic 檢查失敗時，函數直接返回，不執行：
- `set_dirty(index)` 
- `add_to_dirty_pages(header)`
- `record_in_remembered_buffer(header)`

這破壞�了 SATB（Snapshot-At-The-Beginning）擔保，導致 GC 可能錯誤回收仍在使用中的年輕代物件。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 開啟 `crates/rudo-gc/Cargo.toml` 的 `test-util` feature（可選）
2. 執行以下程式碼：

```rust
use rudo_gc::cell::GcCell;
use rudo_gc::{collect_full, Gc, Trace};
use std::cell::RefCell;

#[repr(C)]
struct Container {
    _padding: [u64; 7000],  // 超過一頁的大小
    cell: GcCell<Gc<RefCell<u32>>>,
}

unsafe impl Trace for Container {
    fn trace(&self, visitor: &mut impl rudo_gc::Visitor) {
        self.cell.trace(visitor);
    }
}

fn main() {
    let page_size = rudo_gc::heap::page_size();
    let gc = Gc::new(Container {
        _padding: [0; 7000],
        cell: GcCell::new(Gc::new(RefCell::new(0))),
    });

    // 驗證 GcCell 在第二頁
    let cell_addr = std::ptr::from_ref(&gc.cell) as usize;
    let head_page = (Gc::as_ptr(&gc) as usize) & !page_size;
    let cell_page = cell_addr & !page_size;
    assert_ne!(head_page, cell_page, "GcCell should be in second page");

    collect_full();

    // 年輕代物件
    let young_obj = Gc::new(RefCell::new(12345));

    // 這裡寫屏障應該被觸發，但因為 bug3 會失敗
    *gc.cell.borrow_mut() = young_obj;

    collect_full();

    // 嘗試存取年輕代物件 - 如果 bug 存在，這裡會 UAF
    assert_eq!(*gc.cell.borrow().borrow(), 12345);
}
```

執行測試：
```bash
cargo test --test bug3_write_barrier_multi_page -- --test-threads=1
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：大型物件追蹤（推薦）
在 `GlobalSegmentManager` 中維護大型物件的 header 指標映射：
- 當分配大型物件時，將 header 指標與物件範圍註冊到映射中
- 修改 `ptr_to_page_header()` 優先查詢此映射
- 這種方法最直接，但需要維護額外的資料結構

### 方案 2：頁面類型標記
在頁面元資料中增加標記，指示是否為大型物件的 tail page：
- 在 `PageHeader` 中新增 `is_tail_page: bool` 和 `main_header: NonNull<PageHeader>`
- Tail pages 指回 main header
- 寫屏障可以跟隨此指標找到正確的 header

### 方案 3：改進 magic 檢查
增強 magic 檢查的嚴格性：
- 不僅檢查 magic，還驗證 block_size、obj_count 等欄位的合理性
- 如果驗證失敗，嘗試在大型物件映射中查找

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題源於 BiBOP（Big Bag of Pages）配置與大型物件處理的矛盾。傳統 GC 中，大型物件通常獨立處理（不與一般物件混合），但 rudo-gc 試圖在同一配置中使用兩者。需要明確大型物件的元資料應該存儲在何處，以及如何讓各種指標轉換函數正確處理。

**Rustacean (Soundness 觀點):**
這是 soundness 問題。寫屏障失效破壞了 SATB 擔保，導致記憶體安全的違反。當使用 `GcCell` 存儲 GC 指標時，使用者預期指標在整個生命週期內有效。此 bug 可能在某些情況下導致未定義行為。

**Geohot (Exploit 觀點):**
攻擊者可以通過構造特定大小的物件來控制寫屏障的觸發與否。當 GcCell 位於第二頁時，可以利用此 bug 實現：
1. 任意記憶體寫入（通過 UAF）
2. 資訊洩露（通過操控 GC 回收時機）
3. 類型混淆（通過使物件被錯誤回收）
