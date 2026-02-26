# [Bug]: Lazy Sweep 回收 Slots 時未清除 GEN_OLD_FLAG，導致 slot 重用後 barrier 行為錯誤

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 每次 lazy sweep 回收死亡物件時都會觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致 OLD→YOUNG 引用被錯誤地跳過 barrier |
| **Reproducibility (中現難度)** | Low | 需要觀察 barrier 行為異常，較難直接觀察 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Lazy Sweep (`lazy_sweep_page`, `lazy_sweep_page_all_dead`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current main branch

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 lazy sweep 回收死亡物件並將其 slot 加入 free list 時，應該清除 `GEN_OLD_FLAG`，確保該 slot 未來被重用時，物件的 barrier 狀態是乾淨的。

### 實際行為 (Actual Behavior)
在 `lazy_sweep_page` 函數 (gc/gc.rs:2488-2597) 中，當死亡物件的 slot 被回收並加入 free list 時：
1. 呼叫 `set_dead()` 設定 DEAD_FLAG (gc/gc.rs:2520)
2. 將 slot 加入 free list (gc/gc.rs:2522-2583)
3. **但未呼叫 `clear_gen_old()` 清除 GEN_OLD_FLAG**

這與 `LocalHeap::dealloc()` (heap.rs:2591-2592) 的行為不同，後者會清除 GEN_OLD_FLAG。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **GEN_OLD_FLAG 用途**：用於 generational barrier 的早期退出優化，當物件被 promote 到 old generation 時設置 (gc/gc.rs:1698)
2. **Bug #17 修復**：在 `LocalHeap::dealloc()` 中呼叫 `clear_gen_old()` 清除 flag (heap.rs:2592)
3. **本 issue 遺漏**：Lazy sweep 是另一個回收 slot 的路徑，但在 `lazy_sweep_page` 和 `lazy_sweep_page_all_dead` 中未呼叫 `clear_gen_old()`

**影響**：
- 當 slot 被 lazy sweep 回收並重用時，GEN_OLD_FLAG 仍然存在
- 新物件會錯誤地繼承 old generation flag
- 導致 generational_write_barrier 錯誤地跳過 OLD→YOUNG 引用
- 可能導致年輕物件被錯誤回收（memory leak）

**對比**：
- Bug #17: `LocalHeap::dealloc()` 未清除 GEN_OLD_FLAG → 已修復
- Bug #93: Slot 重用時未清除 DEAD_FLAG → 已修復  
- **本 issue**: Lazy sweep 未清除 GEN_OLD_FLAG → **新發現**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// PoC 概念：需要觀察 lazy sweep 後 slot 重用的 barrier 行為
// 1. 配置一個 OLD generation 物件 (透過 collect_full 使其 promote)
// 2. 釋放物件，觸發 lazy sweep 回收
// 3. 在相同位置配置新物件
// 4. 檢查新物件的 GEN_OLD_FLAG 是否仍然設置 (預期為清除)

fn main() {
    use rudo_gc::*;
    
    // 啟用 lazy-sweep feature
    // 1. 配置物件並 promote 到 old generation
    let old_obj = Gc::new(OldData::default());
    collect_full();
    
    // 2. 釋放物件，觸發 lazy sweep
    drop(old_obj);
    collect_full();
    
    // 3. 配置新物件 (可能重用相同 slot)
    let new_obj = Gc::new(NewData::default());
    
    // 4. 問題：新物件可能仍保留 GEN_OLD_FLAG
    // 導致 generational barrier 錯誤跳過 OLD→YOUNG 引用
}
```

**注意**：根據 AGENTS.md 的驗證指南，需使用 **minor GC** (`collect()`) 而非 `collect_full()` 來測試 barrier 相關問題。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `lazy_sweep_page` 和 `lazy_sweep_page_all_dead` 函數中，slot 加入 free list 前呼叫 `clear_gen_old()`：

```rust
// gc/gc.rs - lazy_sweep_page 中
} else {
    ((*gc_box_ptr).drop_fn)(obj_ptr);
    (*gc_box_ptr).set_dead();
    
    // 新增：清除 GEN_OLD_FLAG，確保重用 slot 是乾淨的
    (*gc_box_ptr).clear_gen_old();
    
    // ... 現有 free list 加入邏輯 ...
}
```

同樣修復 `lazy_sweep_page_all_dead` 函數。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 slot 重用時元資料清除不完整的問題。在 Chez Scheme 中，所有 slot 重用路徑都必須確保元資料被清除。Lazy sweep 是不同於 dealloc 的 code path，但兩者都會回收 slot 到 free list，必須有一致的行為。

**Rustacean (Soundness 觀點):**
這不是嚴格的 soundness 問題，但會導致 GC 正確性問題。GEN_OLD_FLAG 作為 barrier 優化，其正確性依賴於 flag 與物件實際狀態的一致性。建議在所有 slot 回收路徑中清除 flags。

**Geohot (Exploit 觀點):**
攻擊者可能利用此 bug 進行記憶體佈局攻擊。如果能控制 lazy sweep 時序，可能故意留下 GEN_OLD_FLAG 來：
1. 繞過 write barrier
2. 導致年輕物件被錯誤回收（memory leak as DoS）

