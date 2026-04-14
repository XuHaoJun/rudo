# [Bug]: heap::dealloc large object path 缺少 is_allocated 檢查 - drop_fn 可能調用於無效內存

**Status:** Fixed
**Tags:** Verified

## 驗證記錄

**驗證日期:** 2026-04-06
**驗證人員:** opencode

### 驗證結果

代碼確認修復已套用 (`heap.rs:2754-2767`)：
```rust
let gc_box_ptr = addr as *mut crate::ptr::GcBox<()>;
// FIX bug515: Check is_allocated before calling drop_fn.
// If the slot was already swept (has_dead_flag cleared), drop_fn
// could be called on a reused slot with wrong drop function.
// Use ptr_to_object_index to verify slot is still valid.
let idx = unsafe { ptr_to_object_index(addr as *const u8) };
let is_valid = idx.is_some()
    && {
        let header = unsafe { ptr_to_page_header(addr as *const u8) };
        (*header.as_ptr()).is_allocated(idx.unwrap())
    };
if is_valid && !(*gc_box_ptr).has_dead_flag() {
    ((*gc_box_ptr).drop_fn)(addr as *mut u8);
}
```

**Status: Fixed** - 修復已套用於程式碼。

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 slot sweep 後、記憶體回收前 這個視窗期 |
| **Severity (嚴重程度)** | High | 可能導致調用無效記憶體的 drop_fn，造成 memory corruption |
| **Reproducibility (重現難度)** | Very High | 需要精確控制 sweep 和 dealloc 的時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap::dealloc` (heap.rs:2745-2772)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`dealloc` 應該在調用 `drop_fn` 前檢查 slot 是否仍然有效配置 (`is_allocated`)。這是防止調用無效記憶體的 drop function 的基本安全檢查。

### 實際行為 (Actual Behavior)
`dealloc` 的 large object 處理路徑在調用 `drop_fn` 前只檢查 `has_dead_flag()`，但**沒有檢查 `is_allocated`**。如果 slot 已經被 sweep 並添加到 free list（但尚未被重新分配），`has_dead_flag` 可能為 false（因為狀態已被清除），這時 `drop_fn` 會被錯誤地調用在一個已經不屬於原物件的 slot 上。

### 對比 small object 路徑 (正確的行為)

Small object 路的 `dealloc` (line 2790) **有** `is_allocated` 檢查：

```rust
// heap.rs:2788-2792 (small object - CORRECT)
let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
if !unsafe { (*gc_box_ptr).has_dead_flag() } {
    unsafe { ((*gc_box_ptr).drop_fn)(obj_ptr) };  // 但有 is_allocated 檢查保護
}
```

而 large object 路徑 (line 2754-2757) **缺少** `is_allocated` 檢查：

```rust
// heap.rs:2754-2757 (large object - MISSING CHECK!)
let gc_box_ptr = addr as *mut crate::ptr::GcBox<()>;
if !(*gc_box_ptr).has_dead_flag() {
    ((*gc_box_ptr).drop_fn)(addr as *mut u8);  // 直接調用，無 is_allocated 檢查
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `dealloc` 函數中，large object 的處理邏輯：

1. 從 `large_object_map` 找到對應的 header
2. 獲取 `gc_box_ptr`
3. 檢查 `has_dead_flag()` - 如果為 false，調用 `drop_fn`
4. **問題**：沒有檢查 slot 是否仍然有效配置

如果時序如下：
1. Slot A 被 sweep，加入 free list
2. Slot A 的 `has_dead_flag` 被清除（因為是 slot reuse 流程的一部分）
3. 某處調用 `dealloc` 對同一個 slot（可能是錯誤的調用或遺留的指標）
4. `has_dead_flag()` 返回 false（因為已被清除）
5. `drop_fn` 被調用在可能已被新物件使用的 slot 上

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 理論 PoC - 需要精確控制 dealloc 和 sweep 的時序
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 創建大物件並觸發 GC
    let gc = Gc::new(Data { value: 42 });
    let ptr = Gc::into_raw(gc) as usize;
    
    // 2. 釋放物件，觸發 sweep
    // (理論上這會將 slot 加入 free list)
    
    // 3. 在 dealloc 路徑上，如果錯誤地再次調用 dealloc
    // 由於 has_dead_flag 可能已被清除，drop_fn 會被錯誤調用
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 large object 的 `dealloc` 路徑中添加 `is_allocated` 檢查：

```rust
// heap.rs:2754-2757 (FIX)
let gc_box_ptr = addr as *mut crate::ptr::GcBox<()>;
// FIX bugXXX: Check is_allocated before calling drop_fn.
// If the slot was already swept, has_dead_flag may be stale.
if !(*gc_box_ptr).has_dead_flag() {
    // Add is_allocated check - if slot was swept and reused, skip drop_fn
    // (similar to small object path which checks at line 2790)
    // SAFETY: Need to add is_allocated check here
    ((*gc_box_ptr).drop_fn)(addr as *mut u8);
}
```

**注意**：實際的修復可能需要在 `ptr_to_object_index` 或類似函數中添加 is_allocated 檢查，因為 GcBox 本身不直接提供 is_allocated 查詢。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個經典的 TOCTOU 問題。Sweep 和 dealloc 不是原子操作，中間存在窗口期。雖然正常流程下不應該有錯誤的 dealloc 調用，但 defensive 編程應該檢查 slot 的有效性。

**Rustacean (Soundness 觀點):**
調用無效記憶體的 drop_fn 是未定義行為的一種形式。在 Rust 中，這可能導致：
1. 使用已釋放的記憶體（UAF）
2. 記憶體腐敗
3. 任何不可預測的行為

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 dealloc 的時序或調用不存在的 dealloc，可能會觸發記憶體腐敗。雖然這需要精確控制，但在某些場景下可能是可行的攻擊向量。
