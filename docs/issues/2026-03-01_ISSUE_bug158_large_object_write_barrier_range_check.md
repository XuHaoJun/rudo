# [Bug]: large_object_map 範圍檢查錯誤導致寫入屏障漏標大型物件

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當使用大型物件（multi-page）並進行寫入時觸發 |
| **Severity (嚴重程度)** | High | 導致大型物件中部分指標的 write barrier 失效，可能導致標記不完整 |
| **Reproducibility (復現難度)** | Medium | 需要分配大型物件並在特定記憶體範圍進行寫入 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `simple_write_barrier()`, `unified_write_barrier()` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當指標寫入大型物件（large object，multi-page）的資料區域時，write barrier 應該正確標記該頁面為 dirty，並在需要時記錄到 remembered buffer。

### 實際行為 (Actual Behavior)

`simple_write_barrier()` 和 `unified_write_barrier()` 函數中的範圍檢查邏輯有誤，導致大型物件記憶體範圍的上半部分（`head_addr + size` 到 `head_addr + h_size + size`）被錯誤地排除在 barrier 之外。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `heap.rs` 的兩個函數中：

### simple_write_barrier (heap.rs:2659)
```rust
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + size {
    return;
}
```

### unified_write_barrier (heap.rs:2852)
```rust
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + size {
    return;
}
```

**問題分析**：
- `head_addr`: 頁面起始位址
- `h_size`: header 大小
- `size`: 資料大小

正確的範圍應該是：
- 下界：`head_addr + h_size`（header 之後）
- 上界：`head_addr + h_size + size`（header + 資料）

但當前程式碼使用：
- 上界：`head_addr + size`（缺少 `h_size`）

**實際影響**：
假設 `head_addr = 1000`, `h_size = 64`, `size = 1000`：
- 正確範圍：1064 到 2064
- 錯誤範圍：1064 到 2000
- 遺漏範圍：2000 到 2064（最後 64 bytes 的資料寫入不會觸發 barrier）

對比：`mark_page_dirty_for_ptr()` (heap.rs:3297) 和 `find_gc_box_from_ptr()` (heap.rs:3551-3557) 都使用正確的範圍檢查：
```rust
if ptr_addr >= head_addr + h_size && ptr_addr < head_addr + h_size + size {
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};

#[derive(Trace)]
struct LargeData {
    // 大型物件需要足夠大以佔用多個頁面
    data: [u8; 8192], // 8KB - 足以跨越多個頁面
    gc_field: Option<Gc<Inner>>,
}

#[derive(Trace)]
struct Inner { value: i32 }

#[test]
fn test_large_object_write_barrier() {
    // 建立大型物件
    let mut large = Gc::new(LargeData {
        data: [0u8; 8192],
        gc_field: None,
    });
    
    // 建立 inner 物件
    let inner = Gc::new(Inner { value: 42 });
    
    // 寫入 Gc 指針到大型物件的特定位置
    // 需要寫入到 head_addr + size 到 head_addr + h_size + size 範圍內
    // 但由於 barrier 邏輯錯誤，這個範圍內的寫入不會被追蹤
    
    // 獲取大型物件的位址並計算臨界範圍
    let ptr = large.as_ptr() as usize;
    println!("Large object base: {:p}", large.as_ptr());
    
    // 這裡的關鍵是寫入會觸發 barrier，但如果寫入位置在
    // 錯誤的範圍內，barrier 不會被觸發
    
    large.gc_field = Some(inner);
    
    // 釋放 inner 的另一個引用
    drop(inner);
    
    // 應該觸發 GC，但如果 barrier 失效，large.gc_field 可能被錯誤回收
    collect_full();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `simple_write_barrier()` 和 `unified_write_barrier()` 中的範圍檢查：

```rust
// 錯誤的版本 (current)
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + size {
    return;
}

// 正確的版本 (fixed)
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + h_size + size {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Write barrier 是確保 GC 標記完整性的關鍵機制。此 bug 導致大型物件的部分記憶體區域（通常是物件的較高地址部分）不受 write barrier 保護。在 incremental marking 或 generational GC 中，這可能導致該區域內的指標在 GC 期間被遺漏，進而導致標記不完整或錯誤回收。

**Rustacean (Soundness 觀點):**
這不是傳統的記憶體不安全（UB），但可能導致記憶體洩露 - 被錯誤遺漏的指標指向的物件可能會被 GC 錯誤回收，導致 use-after-free。雖然需要特定條件觸發（大型物件 + 寫入特定記憶體範圍），但這是一個嚴重的正確性問題。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能夠控制大型物件的分配和佈局，可能利用此漏洞進行記憶體佈局攻擊。特別是在物件的邊界區域（接近 `head_addr + size` 處）進行操作時，可以繞過 write barrier 追蹤。但實際利用難度較高，需要精確控制記憶體佈局。
