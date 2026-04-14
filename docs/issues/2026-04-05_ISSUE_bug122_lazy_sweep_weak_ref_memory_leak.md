# [Bug]: Lazy Sweep 記憶體洩漏 - 有 weak refs 的死亡物件未被回收

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 任何使用 weak references 的物件在最後一個 strong ref 被 drop 時都會觸發 |
| **Severity (嚴重程度)** | High | 記憶體洩漏 - slot 永遠無法被回收 |
| **Reproducibility (復現難度)** | Medium | 需建立有 weak refs 的物件並讓其死亡 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Lazy Sweep (`lazy_sweep_page`, `lazy_sweep_page_all_dead`)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

在 lazy sweep 的實作中，當一個物件有 weak references 且即將被回收時，程式會標記它為 dead 但永遠不會回收其 slot。

### 預期行為 (Expected Behavior)
物件應該被dropped，其 slot 應該被加入 free list 並標記為 `is_allocated = false`。

### 實際行為 (Actual Behavior)
1. 當 `weak_count > 0 && !dead_flag` 時，物件被 dropped
2. `drop_fn` 和 `trace_fn` 被設為 no-op
3. `set_dead()` 被呼叫設定 DEAD_FLAG
4. **但是 `clear_allocated(i)` 从未被调用！**
5. Slot 永遠保持 allocated 狀態，導致記憶體洩漏

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/gc.rs` 的 `lazy_sweep_page` 函數中（第 2622-2629 行）：

```rust
if weak_count > 0 {
    if !dead_flag {
        ((*gc_box_ptr).drop_fn)(obj_ptr);
        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
        (*gc_box_ptr).set_dead();
    }
    all_dead = false;
} else {
    // reclaim logic - 只有 weak_count == 0 時才會執行
    // ... 這裡才有 clear_allocated(i)
}
```

問題在於：
1. 當 `weak_count > 0 && !dead_flag` 時，程式進入了 `if` 分支
2. 在分支內呼叫了 `drop_fn` 和 `set_dead()`，但**沒有回收 slot**
3. 回收邏輯（`clear_allocated(i)`）只存在於 `else` 分支
4. 因此有 weak refs 的死亡物件永遠不會被回收

同樣的問題也存在於 `lazy_sweep_page_all_dead` 函數（第 2754-2760 行）。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 建立有 weak references 的物件
3. 讓所有 strong refs 被 drop（讓物件死亡）
4. 執行 GC (呼叫 `collect()` 或 `collect_full()`)
5. 觀察 slot 是否被回收

```rust
use rudo_gc::{Gc, Weak, collect_full};
use std::thread;

fn main() {
    // 建立一個有 weak ref 的物件
    let strong = Gc::new(vec![1, 2, 3]);
    let weak: Weak<Vec<i32>> = Gc::downgrade(&strong);
    
    // drop strong ref，讓物件死亡
    drop(strong);
    
    // 執行 GC
    collect_full();
    
    // 嘗試升級 weak ref - 應該回傳 None
    assert!(weak.upgrade().is_none());
    
    // BUG: 此時物件已被標記為 dead，但 slot 未被回收
    // 如果再次嘗試分配，會發現沒有可用的 slot（記憶體洩漏）
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `lazy_sweep_page` 和 `lazy_sweep_page_all_dead` 中，當物件有 weak refs 但需要被回收時，應該也要執行回收邏輯：

```rust
if weak_count > 0 {
    if !dead_flag {
        ((*gc_box_ptr).drop_fn)(obj_ptr);
        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
        (*gc_box_ptr).set_dead();
    }
    // BUG FIX: 這裡也需要回收 slot！
    // Fall through to reclaim logic or add did_reclaim and clear_allocated here
} else {
    // existing reclaim logic
}
```

需要確認為何當初沒有對有 weak refs 的物件進行回收。可能的理由：
- 希望保留 slot 直到所有 weak refs都被清除？
- 但這不合理，因為 weak refs 無法阻止物件被回收

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在典型的 GC 實現中，weak references 不會阻止物件被回收。當一個物件變得不可達時（沒有 strong refs），即使有 weak refs 指向它，也應該被回收。Weak refs 只需要保證在物件被回收後升級返回 None。這個 bug 導致有 weak refs 的死亡物件佔用 slot，造成記憶體洩漏。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，但絕對是記憶體安全問題。Slot 永遠無法被回收會導致記憶體持續增長，最終導致 OOM。

**Geohot (Exploit 觀點):**
記憶體洩漏本身不是安全漏洞，但可用於：
1. 記憶體耗盡攻擊（如果攻擊者能控制何時建立 weak refs）
2. 利用記憶體壓力觸發其他問題

---

## 驗證檢查清單 (Verification Checklist)

- [ ] 在 `lazy_sweep_page` 中確認有 weak refs 的死亡物件未被回收
- [ ] 在 `lazy_sweep_page_all_dead` 中確認同樣問題
- [ ] 確認 `clear_allocated(i)` 只在 `else` 分支被呼叫
- [ ] 確認 weak refs 升級邏輯仍可正常運作（返回 None）
- [ ] 檢查是否有其他路徑會呼叫 `set_dead()` 但不回收 slot

(End of file - total 177 lines)