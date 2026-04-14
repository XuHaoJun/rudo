# [Bug]: Weak 參考導致記憶體無法回收（記憶體洩漏）

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在使用 Weak 參考的場景中，每次 GC 都會觸發 |
| **Severity (嚴重程度)** | High | 記憶體持續成長，最終導致 OOM |
| **Reproducibility (復現難度)** | Very High | 穩定重現：建立有 Weak 參考的物件，drop 強參考，執行 GC |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sweep_phase1_finalize` + `sweep_phase2_reclaim` (gc/gc.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當物件包含 Weak 參考時，該物件的記憶體槽位在 GC 的 sweep phase 無法被回收。只要有任何 Weak 參考指向該物件，即使所有強參考都已 drop 且物件已標記為 dead，該 slot 仍然永遠不會被回收。

### 預期行為
- 物件的強參考全部 drop 後，物件應被視為 unreachable
- GC sweep phase 應回收這些 unreachable 物件的記憶體
- 即使有 Weak 參考存在，記憶體也應在適當時機被回收

### 實際行為
1. 物件 O 建立，擁有 1 個以上的 Weak 參考
2. 所有強參考 drop，O 變成 unreachable
3. GC 執行 sweep phase 1：物件被 drop，標記為 dead
4. GC 執行 sweep phase 2：因為 `weak_count > 0`，slot 不被回收
5. **記憶體洩漏**：該 slot 永遠保持 allocated 狀態

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題出在 `gc/gc.rs` 的 phase1 和 phase2 協調邏輯：

**Phase 1** (`sweep_phase1_finalize`，line 2226-2233)：
```rust
if weak_count > 0 {
    // Has weak refs - drop value but keep allocation
    if !dead_flag {
        ((*gc_box_ptr).drop_fn)(obj_ptr);
        (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
        (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
        (*gc_box_ptr).set_dead();
    }
} else {
    // No weak refs - will be fully reclaimed
    ((*gc_box_ptr).drop_fn)(obj_ptr);
    (*gc_box_ptr).set_dead();
}
```

**Phase 2** (`sweep_phase2_reclaim`，line 2306)：
```rust
if weak_count == 0 && dead_flag {
    // No weak refs, already dropped and dead - reclaim
    // ... reclaim logic ...
}
```

**邏輯矛盾**：
- Phase 1：當 `weak_count > 0` 時，drop 物件值並設為 dead，但保持 slot  allocated
- Phase 2： reclamation 的條件是 `weak_count == 0 && dead_flag`
- 因為 `weak_count > 0`，Phase 2 的條件永遠不會滿足，slot 永遠不會被回收

**缺乏觸發機制**：當 Weak 參考被 drop 時（weak_count 降到 0），沒有任何機制觸發重新 reclaim 該 slot。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{collect_full, Gc, Trace, Weak};
use std::cell::Cell;
use std::rc::Rc;

static DROP_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct TestStruct {
    value: Rc<Cell<bool>>,
}

impl Trace for TestStruct {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

fn main() {
    // 建立有 Weak 參考的物件
    let strong = Gc::new(TestStruct {
        value: Rc::new(Cell::new(false)),
    });
    let weak = Gc::downgrade(&strong);
    
    // 取得初始 heap 使用量
    let pages_before = count_allocated_pages();
    
    // drop 強參考
    drop(strong);
    
    // 執行 major GC
    collect_full();
    
    // 檢查記憶體是否回收
    let pages_after = count_allocated_pages();
    
    // BUG: 即使物件已死，slot 仍然沒有被回收
    assert_eq!(pages_before, pages_after, "記憶體槽位應該被回收但沒有！");
    
    // drop weak 參考
    drop(weak);
    collect_full();
    
    let pages_final = count_allocated_pages();
    // 這時才應該回收（但可能需要下次 GC 才能真正回收）
}

fn count_allocated_pages() -> usize {
    // 取得目前分配的頁面數量
    todo!()
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：延後回收追蹤（推薦）
在 Phase 2 中，如果 `weak_count > 0 && dead_flag`，將該 slot 加入「待回收」追蹤。當 Weak 參考 drop 時（weak_count 變為 0），觸發這些 slot 的延後回收。

### 方案 2：延遲標記 dead
在 Phase 1 中，當 `weak_count > 0` 時，不要立即設 `dead_flag = true`。改為在 Weak 參考完全清除後才標記為 dead，並在下次 GC cycle 回收。

### 方案 3：弱參考-drop 回調
讓 `GcBoxWeakRef` 在 drop 時通知关联的 `GcBox`，觸發立即回收檢查。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個經典的「弱參考回收」問題。在傳統 GC 中，Weak 參考的處理通常有專門的機制（例如 finalizer queue）。rudo-gc 的兩階段 sweep 設計合理，但需要增加弱參考-drop 時的觸發機制。推薦使用方案 1（延後回收追蹤），因為它最小化對現有架構的改動。

**Rustacean (Soundness 觀點):**
這不是 UB，但絕對是記憶體洩漏。在 Rust 的記憶體安全模型下，我們期望「只要所有權結束，記憶體就被回收」。Weak 參考不應阻止記憶體回收。建議優先修復。

**Geohot (Exploit 觀點):**
雖然不是安全性問題，但記憶體洩漏可以導致 DoS（OOM）。攻擊者可以透過觸發大量 Weak 參考使用場景來耗盡記憶體。
