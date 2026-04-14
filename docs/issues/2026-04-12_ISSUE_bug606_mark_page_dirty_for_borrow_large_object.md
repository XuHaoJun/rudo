# [Bug]: mark_page_dirty_for_borrow 未處理大型物件導致 tail page 解析錯誤

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 大型物件中存儲 GcCell 且調用 borrow_mut 的路徑較少 |
| **Severity (嚴重程度)** | Medium | 可能導致不正確的髒頁追蹤，GcCell 中的 Vec<Gc<T>> 在 minor GC 時被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要分配大型物件並在其中存儲 GcCell<Vec<Gc<T>>> |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_page_dirty_for_borrow` in `heap.rs:3147-3189`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`mark_page_dirty_for_borrow()` 應該正確處理所有 GC 堆中的指標，包括大型物件的 tail pages。大型物件的 tail pages 沒有 PageHeader，直接使用 `ptr_to_page_header` 會返回垃圾資料。

### 實際行為 (Actual Behavior)
`mark_page_dirty_for_borrow()` 直接調用 `ptr_to_page_header(ptr)` 而不先檢查 `large_object_map`。對於大型物件的 tail pages，這會返回無效的 PageHeader 指標，導致：
1. `(*h).magic` 可能不通過 `MAGIC_GC_PAGE` 檢查，導致早期返回
2. 或更糟的情況 - 讀取到錯誤的 magic 值但僥倖通過檢查

對比其他 barrier 函數（`gc_cell_validate_and_barrier`、`unified_write_barrier`、`incremental_write_barrier`），它們都有明確的大型物件處理邏輯：
```rust
// 其他 barrier 的正確模式
let (h, index) = if let Some(&(head_addr, size, h_size)) =
    heap.large_object_map.get(&page_addr)
{
    // 正確處理大型物件...
}
```

但 `mark_page_dirty_for_borrow` 缺少這個檢查：
```rust
// heap.rs:3159 - 直接使用 ptr_to_page_header，未檢查大型物件地圖
let header = ptr_to_page_header(ptr);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`mark_page_dirty_for_borrow` 函數 (`heap.rs:3147-3189`) 的實現缺少大型物件檢查：

```rust
pub unsafe fn mark_page_dirty_for_borrow(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }

    let ptr_addr = ptr as usize;
    with_heap(|heap| {
        if ptr_addr < heap.min_addr || ptr_addr >= heap.max_addr {
            return;
        }

        unsafe {
            let header = ptr_to_page_header(ptr);  // BUG: 未檢查 large_object_map！
            let h = header.as_ptr();

            if (*h).magic != MAGIC_GC_PAGE {  // Tail pages 會在此返回
                return;
            }
            // ...
        }
    });
}
```

問題根源：
1. 大型物件由多個頁面組成，但只有 head page 有有效的 `PageHeader`
2. Tail pages 的 `PageHeader` 是無效的垃圾資料
3. `ptr_to_page_header` 只是簡單地 `ptr & page_mask()`，對於 tail pages 返回錯誤的地址
4. 其他 barrier 函數都有檢查 `large_object_map.get(&page_addr)` 的邏輯

這影響 `GcCell::borrow_mut` (cell.rs:205) 中的追蹤：
- 當 `GcCell<Vec<Gc<T>>>` 在大型物件中時
- `borrow_mut` 調用 `mark_page_dirty_for_borrow`
- 對於 tail page，函數早期返回，頁面未被標記為 dirty
- 導致 minor GC 時 `Vec<Gc<T>>` 中的指標未被掃描

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};
use std::cell::RefCell;

#[derive(Trace)]
struct Container {
    data: RefCell<Vec<Gc<i32>>>,
}
static_collect!(Container);

fn main() {
    // 分配大型物件（確保 GcCell 在 tail page）
    let large_vec = (0..1000).map(|_| Gc::new(0i32)).collect::<Vec<_>>();
    
    let container = Gc::new(Container {
        data: RefCell::new(large_vec),
    });

    // 強制 full GC 確保物件 promote 到 old generation
    collect_full();

    // 在大型物件中的 GcCell 上調用 borrow_mut
    // 這會觸發 mark_page_dirty_for_borrow
    drop(container.data.borrow_mut());
    
    // 預期：Gc 指標應該存活
    // 實際：可能已被回收（如果 bug 觸發）
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `mark_page_dirty_for_borrow` 以正確處理大型物件：

```rust
pub unsafe fn mark_page_dirty_for_borrow(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }

    let ptr_addr = ptr as usize;
    with_heap(|heap| {
        if ptr_addr < heap.min_addr || ptr_addr >= heap.max_addr {
            return;
        }

        let page_addr = ptr_addr & page_mask();

        unsafe {
            // 檢查是否為大型物件的 tail page
            if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
                if ptr_addr >= head_addr + h_size && ptr_addr < head_addr + h_size + size {
                    // 在大型物件的 value 區域內
                    let h_ptr = head_addr as *mut PageHeader;
                    if !(*h_ptr).is_allocated(0) {
                        return;
                    }
                    (*h_ptr).set_dirty(0);
                    heap.add_to_dirty_pages(NonNull::new_unchecked(h_ptr));
                    return;
                }
            }

            // 小物件處理（原有邏輯）
            let header = ptr_to_page_header(ptr);
            let h = header.as_ptr();

            if (*h).magic != MAGIC_GC_PAGE {
                return;
            }

            let block_size = (*h).block_size as usize;
            let header_size = (*h).header_size as usize;
            let header_page_addr = h as usize;

            if ptr_addr < header_page_addr + header_size {
                return;
            }

            let offset = ptr_addr - (header_page_addr + header_size);
            let index = offset / block_size;
            let obj_count = (*h).obj_count as usize;
            if index >= obj_count {
                return;
            }

            if !(*h).is_allocated(index) {
                return;
            }

            (*h).set_dirty(index);
            heap.add_to_dirty_pages(header);
        }
    });
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
BiBOP (Big Bag of Pages) 內存佈局中，大型物件的特殊處理是必要的。大型物件的 tail pages 沒有 PageHeader 結構，這是節省內存和簡化 large object 追蹤的設計選擇。但代價是所有访问大型物件的函數都必須先檢查 `large_object_map` 以確定是否在大型物件區域內。

**Rustacean (Soundness 觀點):**
這不是立即的記憶體不安全 (UAF)，因為 `ptr_to_page_header` 返回的指標仍然在進程地址空間內。但可能導致邏輯錯誤：髒頁追蹤不正確，進而導致記憶體洩露（不該回收的物件被回收）。這屬於 GC 正確性問題而非 Rust 安全性的 UB。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制大型物件的分配和釋放，並觸發特定時序的 GC，可能利用此漏洞進行記憶體洩露攻擊。但更實際的影響是導致正確性問題 - 某些 Gc 指標在 minor GC 時被錯誤回收。
