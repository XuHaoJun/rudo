# [Bug]: promote_young_pages and promote_all_pages use BITMAP_SIZE instead of obj_count - buffer overrun

**Status:** Fixed
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要 bitmap 不一致（某處 bug 導致超出 obj_count 的 bit 被設定） |
| **Severity (嚴重程度)** | `Critical` | 可能導致記憶體存取越界，緩衝區溢出 |
| **Reproducibility (重現難度)** | `Medium` | 需要觸發 bitmap 不一致，可能難以穩定重現 |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `promote_young_pages` (gc/gc.rs:1712-1723), `promote_all_pages` (gc/gc.rs:2356-2367)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.x`

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

在 `promote_young_pages` 和 `promote_all_pages` 中，遍歷 bitmap 設置 `GEN_OLD_FLAG` 時，應該只處理有效物件範圍內的 slots。有效範圍由 `obj_count` 指定。

### 實際行為 (Actual Behavior)

兩個函數都使用 `BITMAP_SIZE` (64 words = 4096 possible slots) 來遍歷 bitmap，而不是 `obj_count`：

```rust
// gc/gc.rs:1712-1723 (promote_young_pages)
for word_idx in 0..crate::heap::BITMAP_SIZE {  // Bug: 遍歷所有 64 words
    let bits = (*header).allocated_bitmap[word_idx].load(Ordering::Acquire);
    let mut b = bits;
    while b != 0 {
        let bit_idx = b.trailing_zeros() as usize;
        let obj_idx = word_idx * 64 + bit_idx;
        // Bug: 沒有檢查 obj_idx >= obj_count！
        let gc_box_addr = (page_addr + header_size + obj_idx * block_size)
            as *const crate::ptr::GcBox<()>;
        (*gc_box_addr).set_gen_old();  // 可能存取超過 page 邊界！
        b &= b - 1;
    }
}
```

### 計算範例

對於 4KB page (4096 bytes) + 16-byte blocks：
- `header_size ≈ 64` bytes
- `obj_count = (4096 - 64) / 16 = 252` objects

但 `BITMAP_SIZE = 64`允許 `obj_idx` 達到 `63 * 64 + 63 = 4095`。

如果 `obj_idx = 300` (超過 obj_count=252)：
- `gc_box_addr = page_addr + 64 + 300 * 16 = page_addr + 4864`
- 超出 page 邊界 768 bytes (緩衝區溢出)！

---

## 根本原因分析 (Root Cause Analysis)

1. **Bitmap 設計**：每個 page 的 header 包含 `allocated_bitmap[64]` (64 words × 64 bits = 4096 slots)，這是為了支援未來的頁面合併和記憶體管理優化。

2. **obj_count 限制**：`obj_count` 才是實際可容納的物件數量。對於 4KB page + 16-byte blocks，只有 252 個可用 slots。

3. **一致性問題**：正確的程式碼 (如 `is_fully_marked` 在 heap.rs:3298-3299) 使用 `(0..obj_count).any(...)` 來遍歷，但 `promote_young_pages` 和 `promote_all_pages` 使用 `BITMAP_SIZE`。

4. **觸發條件**：如果某處有 bug 導致超出 `obj_count` 的 bit 被錯誤設定，就會觸發此問題。正常情況下這些 bits 不應被設定。

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要先有一個導致 bitmap 不一致的 bug 才能觸發。正常情況下：
- 分配時設定 bitmap bits
- 釋放時清除 bitmap bits
- 如果某處有 bug 導致 bit 未被清除，就會導致存取越界

---

## 建議修復方案 (Suggested Fix / Remediation)

在迴圈中加入 `obj_idx >= obj_count` 檢查：

```rust
let obj_count = (*header).obj_count as usize;
for word_idx in 0..crate::heap::BITMAP_SIZE {
    let bits = (*header).allocated_bitmap[word_idx].load(Ordering::Acquire);
    let mut b = bits;
    while b != 0 {
        let bit_idx = b.trailing_zeros() as usize;
        let obj_idx = word_idx * 64 + bit_idx;
        // FIX: 跳過超出 obj_count 的 slots
        if obj_idx >= obj_count {
            b &= b - 1;
            continue;
        }
        let gc_box_addr = (page_addr + header_size + obj_idx * block_size)
            as *const crate::ptr::GcBox<()>;
        (*gc_box_addr).set_gen_old();
        b &= b - 1;
    }
}
```

同樣的修復應用於 `promote_all_pages` (gc/gc.rs:2356-2367)。

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Bitmap 的大小 (64 words) 是預留空間用於未來的頁面合併等優化。使用 BITMAP_SIZE 遍歷在正常情況下是安全的（因為超出 obj_count 的 bits 不應被設定），但缺乏防御性檢查是一個潛在的緩衝區溢出風險。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。即使正常情況下不會觸发，但一旦發生（由於其他 bug 或記憶體損壞），會導致緩衝區溢出。防御性編碼原則要求加入邊界檢查。

**Geohot (Exploit 觀點):**
如果攻擊者能夠操控記憶體（例如透過其他漏洞），可能會設置超出 obj_count 的 bitmap bits，導致此代碼路徑成為記憶體損壞的攻擊向量。

---

## Resolution (2026-03-28)

`promote_young_pages` and `promote_all_pages` in `gc/gc.rs` load `obj_count` from the page header and skip indices with `obj_idx >= obj_count` inside the bitmap walk, matching the suggested fix above. Issue closed as implemented in tree.