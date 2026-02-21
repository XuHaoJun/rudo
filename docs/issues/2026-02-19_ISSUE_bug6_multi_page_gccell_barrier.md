# [Bug]: Multi-Page Large Object 的 GcCell Write Barrier 在 Tail Pages 上失效

**Status:** Open
**Tags:** Not Reproduced


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在大型物件（>1 page）的第二頁或後續頁面分配 GcCell |
| **Severity (嚴重程度)** | Critical | SATB 不變性被破壞，導致 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要特定的大小和配置來觸發跨頁分配 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc_cell_validate_and_barrier`, `unified_write_barrier`, `ptr_to_page_header`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當 `GcCell` 被分配在大型物件（>1 page）的第二頁或後續頁面（tail pages）時，write barrier 會失效。這是因為：

1. `ptr_to_page_header()` 將指標 Mask 到 page boundary
2. 大型物件的 tail pages 沒有 `PageHeader` - 它們只包含用戶數據
3. Magic check 失敗（讀取到隨機垃圾數據），barrier 提前返回
4. SATB 不變性被破壞，導致潛在的 use-after-free

### 預期行為
- 當 OLD 物件的 GcCell（在任何頁面上）寫入年輕代指標時，應該觸發 generational/incremental write barrier
- 引用應該被正確記錄到 dirty pages

### 實際行為
- Tail page 上的 GcCell 的 magic check 失敗
- Barrier 提前返回，不記錄引用
- 年輕代物件可能被錯誤回收，導致 use-after-free

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs:2556-2628` 的 `gc_cell_validate_and_barrier` 函數中：

```rust
unsafe {
    let header = ptr_to_page_header(ptr);  // 問題在這裡
    let h = header.as_ptr();

    if (*h).magic != MAGIC_GC_PAGE {  // Tail pages 沒有有效的 magic
        return;  // Barrier 提前返回！
    }
    // ...
}
```

問題在於：
1. `ptr_to_page_header()` 使用 page mask（4KB 或更大）來獲取頁面起始地址
2. 對於大型物件，只有第一頁有 `PageHeader`
3. Tail pages 沒有 header，讀取到的 `magic` 是隨機數據
4. Magic check 失敗導致 barrier 不執行

相同的問題也存在於 `unified_write_barrier` (`heap.rs:2637-2687`)。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};

#[derive(Trace)]
struct LargeData {
    _padding: [u8; 8192],  // 超過一個頁面大小
    cell: GcCell<i32>,
}

fn main() {
    // 創建大型資料，確保 GcCell 在第二頁
    let data = Gc::new(LargeData {
        _padding: [0xAA; 8192],
        cell: GcCell::new(0),
    });

    // 創建年輕代資料
    let young = Gc::new(42);

    // 多次 GC 觸發 promotion
    for _ in 0..10 {
        collect_full();
    }

    // 執行 OLD → YOUNG 寫入
    // 這裡 barrier 應該被觸發，但因為 GcCell 在 tail page 上所以不會
    *data.cell.borrow_mut() = 100;

    // Minor GC
    collect_full();

    // 嘗試存取 - 如果 bug 存在，可能 UAF
    println!("{}", *data.cell.borrow());
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：為 Large Object 維護獨立的追蹤機制（推薦）

為大型物件維護一個從 object address 到 header address 的映射：

```rust
// 在 heap.rs 中
pub struct LargeObjectEntry {
    pub header_addr: usize,
    pub size: usize,
}

// large_object_map 已經存在，但需要添加反向映射
// 從任意 object address 可以找到 header
pub fn get_large_object_header(ptr: *const u8) -> Option<*mut PageHeader> {
    let ptr_addr = ptr as usize;
    let page_addr = ptr_addr & page_mask();
    
    if let Some(&(header_addr, _, _)) = large_object_map.get(&page_addr) {
        Some(header_addr as *mut PageHeader)
    } else {
        // 嘗試從 tail page 的 address 找到
        // 需要扫描映射表
        None
    }
}
```

### 方案 2：在 Barrier 中添加大型物件特殊處理

在 `gc_cell_validate_and_barrier` 和 `unified_write_barrier` 中：

```rust
let header = ptr_to_page_header(ptr);

// 先檢查是否是大型物件的 tail page
if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
    // 嘗試在 large_object_map 中查找
    let page_addr = ptr_addr & page_mask();
    if let Some(&(header_addr, _, _)) = heap.large_object_map.get(&page_addr) {
        // 使用 header_addr 作為真正的 header
        let h = header_addr as *mut PageHeader;
        // ... 執行 barrier 邏輯
        return;
    }
    return; // 仍然提前返回，但不是因為 bug
}
```

### 方案 3：確保 GcCell 永遠在第一頁

修改分配邏輯，確保 `GcCell` 永遠在大型物件的第一頁：

```rust
// 在分配時調整佈局
struct LargeData {
    cell: GcCell<i32>,  // 確保在前面
    _padding: [u8; 8192],
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題反映了 BiBOP 配置中的一個常見陷阱：多頁面物件的元數據管理。在傳統的 GC 中，大型物件通常作為單一連續區塊管理，不會有 tail pages 的概念。rudo-gc 的實現需要特別處理這種情況，確保所有物件的元數據都可以被正確訪問。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。當 write barrier 失效時，SATB 不變性被破壞，導致物件可能被錯誤回收。這是未定義行為，可能導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過：
1. 構造特定大小的大型物件
2. 控制 GC 時機
3. 利用 barrier 失效實現記憶體佈局控制
4. 最終可能實現任意記憶體讀寫

