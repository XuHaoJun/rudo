# [Bug]: lazy_sweep_page missing clear_under_construction and clear_is_dropping causes stale flag leak on slot reuse

**Status:** Closed
**Tags:** Verified, Fixed

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires slot reuse after lazy sweep with objects that had under_construction or is_dropping flags set |
| **Severity (嚴重程度)** | `Medium` | Stale flags can cause incorrect behavior (e.g., is_under_construction preventing marking) in newly allocated objects |
| **Reproducibility (復現難度)** | `Medium` | Need to create scenario where: 1) object has under_construction/is_dropping set, 2) object becomes dead, 3) lazy sweep reclaims slot, 4) new object allocated in same slot |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `lazy_sweep_page` and `lazy_sweep_page_all_dead` in `crates/rudo-gc/src/gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

在 `lazy_sweep_page` 和 `lazy_sweep_page_all_dead` 函數中，當死亡物件的 slot 被回收並加入 free list 時，程式碼只呼叫 `clear_gen_old()`（用於清除 GEN_OLD_FLAG），但**未呼叫** `clear_under_construction()` 和 `clear_is_dropping()`。

這與 `sweep_phase2_reclaim` 函數（在 bug406、bug428 中已修復）的行為不一致。在 `sweep_phase2_reclaim` 中，回收死亡 slot 時會呼叫：
- `clear_dead()`
- `clear_gen_old()`
- `clear_under_construction()`  (bug406 修復)
- `clear_is_dropping()`  (bug428 修復)

### 預期行為 (Expected Behavior)

Lazy sweep 回收 slot 時，應該清除所有可能影響新物件的狀態標誌，包括 `under_construction` 和 `is_dropping`。

### 實際行為 (Actual Behavior)

Lazy sweep 只清除 `gen_old` 標誌，導致 slot 被回收並重新分配後，新物件可能繼承舊的 `under_construction` 或 `is_dropping` 狀態。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 受影響的程式碼

**`lazy_sweep_page_all_dead`** (gc.rs:2752-2755):
```rust
} else {
    ((*gc_box_ptr).drop_fn)(obj_ptr);
    // Clear GEN_OLD_FLAG so reused slots don't inherit stale barrier state (bug135).
    (*gc_box_ptr).clear_gen_old();
    // MISSING: (*gc_box_ptr).clear_under_construction();
    // MISSING: (*gc_box_ptr).clear_is_dropping();
```

**`lazy_sweep_page`** (gc.rs:2622-2626):
```rust
} else {
    ((*gc_box_ptr).drop_fn)(obj_ptr);
    (*gc_box_ptr).set_dead();
    // Clear GEN_OLD_FLAG so reused slots don't inherit stale barrier state (bug135).
    (*gc_box_ptr).clear_gen_old();
    // MISSING: (*gc_box_ptr).clear_under_construction();
    // MISSING: (*gc_box_ptr).clear_is_dropping();
```

### 正確的模式

參考 `sweep_phase2_reclaim` (gc.rs:2293-2296):
```rust
(*header).clear_allocated(i);
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();  // ✓ 清除
(*gc_box_ptr).clear_is_dropping();         // ✓ 清除
```

以及 `LocalHeap::try_pop_from_page` (heap.rs):
```rust
(*gc_box_ptr).clear_dead();
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();
(*gc_box_ptr).clear_is_dropping();
```

### 文件依據

`GcBox::clear_under_construction()` 的文件明確說明：
> "Clear `UNDER_CONSTRUCTION_FLAG`. Used when reusing a slot so the new object is not incorrectly marked as under construction. Must be called before the slot is used for a new allocation"

`GcBox::clear_is_dropping()` 的文件明確說明：
> "Clear `is_dropping`. Used when reusing a slot so the new object does not inherit a dropping state from the previous object. Must be called before the slot is used for a new allocation (bug408)"

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 使用 `Gc::new_cyclic_weak` 創建一個處於 `under_construction` 狀態的對象
2. 確保所有 Gc/Weak 引用都被 drop，使對象死亡
3. 觸發 lazy sweep 回收該 slot
4. 在同一 slot 分配新對象
5. 嘗試標記或追蹤新對象 - 可能因為 `is_under_construction()` 返回 true 而被錯誤跳過

```rust
// 概念驗證（需要更精確的并发控制）
fn poc_stale_under_construction() {
    // 1. Create object that will have under_construction set
    let (gc, weak) = Gc::new_cyclic_weak(|r| Data { ref_: r.clone() });
    
    // 2. Drop all references to make object dead
    drop(gc);
    drop(weak);
    
    // 3. Trigger lazy sweep (may reclaim the slot)
    collect_full();
    
    // 4. Allocate new object in same slot
    let new_gc = Gc::new(Data { ref_: None });
    
    // 5. Problem: new_gc may still have is_under_construction() == true
    //    if the lazy sweep didn't clear the flag
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `lazy_sweep_page` 和 `lazy_sweep_page_all_dead` 的 `weak_count == 0` 分支中，新增 `clear_under_construction()` 和 `clear_is_dropping()` 呼叫：

**gc.rs:2752-2756 (lazy_sweep_page_all_dead):**
```rust
} else {
    ((*gc_box_ptr).drop_fn)(obj_ptr);
    // Clear GEN_OLD_FLAG so reused slots don't inherit stale barrier state (bug135).
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();  // ADD THIS LINE
    (*gc_box_ptr).clear_is_dropping();         // ADD THIS LINE
```

**gc.rs:2622-2626 (lazy_sweep_page):**
```rust
} else {
    ((*gc_box_ptr).drop_fn)(obj_ptr);
    (*gc_box_ptr).set_dead();
    // Clear GEN_OLD_FLAG so reused slots don't inherit stale barrier state (bug135).
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();  // ADD THIS LINE
    (*gc_box_ptr).clear_is_dropping();         // ADD THIS LINE
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 Chez Scheme 的 GC 中，slot 回收時會清除所有與對象生命週期相關的狀態。lazy sweep 作為一個獨立的回收路徑，必須與主 sweep 路徑保持一致的行為。缺少 `clear_under_construction` 和 `clear_is_dropping` 可能導致新分配的對象被錯誤地視為"在建"或"正在 drop"狀態。

**Rustacean (Soundness 觀點):**
如果 `is_under_construction()` 對新對象返回 true，可能導致對象被錯誤地跳過標記，造成 use-after-free。雖然不太可能直接導致 UB，但會造成記憶體安全問題。

**Geohot (Exploit 觀點):**
繼承 `is_dropping` 狀態可能允許攻擊者操縱對象的生命週期控制流程，特別是在涉及 cyclic reference drop 時的 reentrancy 檢查。

---

## 相關 Bug

- bug135: lazy_sweep missing clear_gen_old (已修復)
- bug406: sweep_phase2_reclaim missing clear_under_construction (已修復)
- bug408: slot reuse is_dropping not cleared (已修復)
- bug428: sweep_phase2_reclaim missing clear_is_dropping (已修復)
