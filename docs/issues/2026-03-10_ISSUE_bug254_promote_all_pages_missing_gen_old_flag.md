# [Bug]: promote_all_pages 缺少 GEN_OLD_FLAG 設置導致與 promote_young_pages 行為不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | 每次 major GC 都會發生 |
| **Severity (嚴重程度)** | `Low` | 這是效能問題而非正確性問題（因 bug71 修復後 barrier 會檢查 page generation） |
| **Reproducibility (難度)** | `Low` | 程式碼結構明確，可直接比對 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `promote_all_pages` in `crates/rudo-gc/src/gc/gc.rs`
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在 major GC 後，`promote_all_pages` 應該與 `promote_young_pages` 行為一致：設置 page generation = 1 的同時，也要在每個存活的物件上設置 `GEN_OLD_FLAG`（透過 `set_gen_old()`）。

### 實際行為 (Actual Behavior)
`promote_all_pages` 只設置 `generation = 1`，但沒有調用 `set_gen_old()` 設置 per-object flag。這與 `promote_young_pages` 的行為不一致。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc.rs:2324-2330` 的 `promote_all_pages` 函數：

```rust
/// Promote ALL pages (after Major GC).
fn promote_all_pages(heap: &LocalHeap) {
    for page_ptr in heap.all_pages() {
        unsafe {
            (*page_ptr.as_ptr()).generation = 1;  // 只設置 page generation
            // 缺少：沒有設置 gen_old flag
        }
    }
}
```

相比之下，`promote_young_pages` (gc.rs:1691-1710) 正確地設置了兩者：

```rust
if has_survivors {
    (*header).generation = 1; // Promote!
    // ... 遍歷所有物件 ...
    (*gc_box_addr).set_gen_old();  // 設置 GEN_OLD_FLAG
}
```

`gen_old` flag 是 write barrier 的優化捷徑（見 bug71 修復後的程式碼）。雖然 bug71 修復後 barrier 會先檢查 page generation，使得此問題不會造成正確性問題，但：

1. 行為不一致造成維護困難
2. 缺少了 per-object flag 的優化效果
3. 與程式碼註釋不符："Set GEN_OLD_FLAG on each surviving object for barrier early-exit"

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

這是程式碼結構問題，直接比對即可確認：

1. 比較 `promote_all_pages` (gc.rs:2324-2330) 與 `promote_young_pages` (gc.rs:1691-1710)
2. 觀察 `promote_young_pages` 有設置 `gen_old` flag 的迴圈 (gc.rs:1698-1710)
3. 確認 `promote_all_pages` 缺少這段程式碼

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `promote_all_pages` 中加入設置 `gen_old` flag 的邏輯，與 `promote_young_pages` 保持一致：

```rust
fn promote_all_pages(heap: &LocalHeap) {
    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            (*header).generation = 1;
            
            // Set GEN_OLD_FLAG on each surviving object for barrier optimization
            let block_size = (*header).block_size as usize;
            let header_size = crate::heap::PageHeader::header_size(block_size);
            let page_addr = header as usize;
            
            for word_idx in 0..crate::heap::BITMAP_SIZE {
                let bits = (*header).allocated_bitmap[word_idx].load(Ordering::Acquire);
                let mut b = bits;
                while b != 0 {
                    let bit_idx = b.trailing_zeros() as usize;
                    let obj_idx = word_idx * 64 + bit_idx;
                    let gc_box_addr = (page_addr + header_size + obj_idx * block_size)
                        as *const crate::ptr::GcBox<()>;
                    (*gc_box_addr).set_gen_old();
                    b &= b - 1;
                }
            }
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 consistency 問題。傳統generational GC 在 major GC 後，所有存活物件都應該被視為 OLD 物件。雖然 page-level generation 檢查（bug71 修復）足以保證正確性，但 per-object flag 是重要的優化捷徑。與 `promote_young_pages` 行為不一致會造成預期外的效能損失。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題。Bug71 修復後，write barrier 會先檢查 page generation (`generation > 0`)，因此即使沒有 `gen_old` flag，barrier 仍會正確觸發。這是純效能問題。

**Geohot (Exploit 觀點):**
無額外 exploit 風險。這是內部優化問題，不影響記憶體安全。

---

## 驗證記錄

**驗證日期:** 2026-03-10
**驗證人員:** opencode

### 驗證結果

確認 `promote_all_pages` (gc.rs:2324-2330) 缺少 `gen_old` flag 設置邏輯。

對比：
- `promote_young_pages` (gc.rs:1691-1710): 有設置 `gen_old` flag
- `promote_all_pages` (gc.rs:2324-2330): 沒有設置 `gen_old` flag

**Status: Fixed** - 已修復。`promote_all_pages` 現已包含 `set_gen_old()` 邏輯（gc.rs:2334-2345），與 `promote_young_pages` 行為一致。
