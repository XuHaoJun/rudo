# [Bug]: GEN_OLD_FLAG 在物件釋放時未被清除，導致重新配置後產生錯誤的 barrier 行為

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 物件配置/釋放頻繁發生，每次都會觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致 OLD→YOUNG 引用被錯誤地跳過 barrier |
| **Reproducibility (復現難度)** | Low | 需要觀察 barrier 行為異常，較難直接觀察 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::dealloc` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current main branch

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當物件被釋放（deallocation）時，所有與該物件關聯的元資料應該被清除，包括存放在 `GcBox.weak_count` 欄位中的 `GEN_OLD_FLAG`。

### 實際行為 (Actual Behavior)
在 `LocalHeap::dealloc()` 函數中，物件被釋放並加入 free list，但 `GEN_OLD_FLAG` 並未被清除：

```rust
// 在 heap.rs:2399 的 dealloc 函數中
pub unsafe fn dealloc(&mut self, ptr: NonNull<u8>) {
    // ... 略過大型物件處理 ...
    
    // 小物件處理
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    if !unsafe { (*gc_box_ptr).has_dead_flag() } {
        unsafe { ((*gc_box_ptr).drop_fn)(obj_ptr) };
    }
    
    // 添加回 free list - 但 GEN_OLD_FLAG 仍然存在於 weak_count 中！
    unsafe {
        let mut next_head = (*header).free_list_head();
        obj_ptr.cast::<Option<u16>>().write_unaligned(next_head);
        // ...
    }
}
```

`GEN_OLD_FLAG` 在 promotion 時被設置（在 `gc/gc.rs:1652-1664`），但在 deallocation 時從未被清除。

---

## 🔬 根本原因分析 (Root Cause Analysis)

當具有 `GEN_OLD_FLAG` 的物件被釋放並且相同的記憶體位置後來被重新配置給新物件時，會發生以下問題：

1. **錯誤的 barrier 行為**：世代 write barrier 檢查 `GEN_OLD_FLAG` 以跳過年輕物件的 barrier。如果新配置（年輕）的物件仍保留舊的 flag，barrier 可能會被錯誤地跳過。

2. **追蹤骯髒頁面的假陽性**：應該觸發 barrier 的物件不會觸發，因為 flag 錯誤地表示它們是舊物件。

3. **違反假設**：系統假設每個物件的 `GEN_OLD_FLAG` 正確反映其實際世代，但這在物件重用後不再成立。

這與現有 issue #3（`2026-02-19_ISSUE_bug3_generational_barrier_gen_old_flag.md`）不同：
- Issue #3: Barrier 未檢查 per-object `GEN_OLD_FLAG`
- 本 issue: `GEN_OLD_FLAG` 在物件釋放後未清除，導致重新配置後仍然存在

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
fn main() {
    use rudo_gc::*;
    
    // 1. 配置一個年輕物件
    let obj1 = Gc::new(OldGenerationData::default());
    
    // 2. 觸發 GC，物件被標記為 OLD 並設置 GEN_OLD_FLAG
    collect_full();
    
    // 3. 釋放物件 (drop obj1)
    drop(obj1);
    collect_full();
    
    // 4. 在相同位置配置新物件 (可能使用相同記憶體)
    let obj2 = Gc::new(YoungData::default());
    
    // 5. 問題：新物件可能繼承了舊的 GEN_OLD_FLAG！
    // 這會導致 generational_write_barrier 錯誤地跳過 OLD→YOUNG 引用
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `LocalHeap::dealloc()` 中，物件加入 free list 之前清除 `GEN_OLD_FLAG`：

```rust
// 在 heap.rs 的 dealloc 函數中
let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();

// 清除 GEN_OLD_FLAG (以及其他 flags)
unsafe {
    let weak_count = (*gc_box_ptr).weak_count.load(Ordering::Relaxed);
    (*gc_box_ptr).weak_count.store(
        weak_count & !GcBox::<()>::GEN_OLD_FLAG,
        Ordering::Relaxed
    );
}
```

或者在物件從 free list 重新配置時清除：

```rust
// 在 allocation 時清除 flags
pub unsafe fn allocate(&mut self, size: usize, ..) -> Option<NonNull<u8>> {
    // ... 取得記憶體後 ...
    
    // 清除所有 flags，包括 GEN_OLD_FLAG
    (*gc_box_ptr).weak_count.store(0, Ordering::Relaxed);
    
    Some(ptr)
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是記憶體管理中的一個常見問題：元資料在物件生命週期結束時未被正確清除。在 Chez Scheme 中，我們確保在物件重用前清除所有元資料。這個問題會導致錯誤的 barrier 行為，可能造成記憶體洩漏（因為 OLD→YOUNG 引用未被記錄，導致年輕物件被錯誤回收）。

**Rustacean (Soundness 觀點):**
這不是嚴格的 soundness 問題（不會導致 UB），但會導致 GC 正確性問題。`GEN_OLD_FLAG` 作為一種優化機制，其正確性依賴於 flag 與物件實際狀態的一致性。建議在物件配置時初始化所有 flags 為零，並在釋放時清除 flags。

**Geohot (Exploit 觀點):**
這可能被利用來進行記憶體佈局攻擊。如果攻擊者能控制物件的配置/釋放時機，他們可能故意留下 `GEN_OLD_FLAG` 來：
1. 繞過 write barrier
2. 導致年輕物件被錯誤回收（記憶體洩漏）
3. 進行記憶體佈局預測攻擊

建議添加額外的安全檢查，例如在 barrier 中驗證物件是否真的處於 OLD 世代。
