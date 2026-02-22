# [Bug]: Dirty Pages Snapshot Race 導致 Young 物件被錯誤回收

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需在 minor GC snapshot 後、掃描前建立新的 OLD→YOUNG 引用 |
| **Severity (嚴重程度)** | `Critical` | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | `High` | 需精確時序控制，多執行緒環境下較易觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Minor GC, Dirty Page Tracking, Incremental Marking
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
Minor GC 應該掃描所有 dirty pages，包含在 GC 週期開始後寫入 barrier 新增的 dirty pages，確保所有 OLD→YOUNG 引用都被追蹤，young 物件不會被錯誤回收。

### 實際行為 (Actual Behavior)
在 `take_dirty_pages_snapshot()` 將 `dirty_pages` 移動到 snapshot 後，寫入 barrier 新增的新 dirty pages 不會被包含在當前 GC 週期的掃描中。如果這些新 dirty pages 包含 OLD→YOUNG 引用，可能導致 young 物件被錯誤回收。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：**
1. `heap.rs:1649-1656` - `take_dirty_pages_snapshot()` 在 minor GC 開始時將 `dirty_pages` 排空並移動到 `dirty_pages_snapshot`
2. `heap.rs:1630-1638` - `add_to_dirty_pages_slow()` 將新頁面添加到 `dirty_pages`（不是 snapshot）
3. `gc.rs:1138` - Snapshot 在 minor GC 開始時拍攝
4. `gc.rs:1141` - 只有 `dirty_pages_snapshot` 被掃描

**時序問題：**
```
1. take_dirty_pages_snapshot() → dirty_pages 被清空
2. [時間點 A] 寫入 barrier 執行 → add_to_dirty_pages() → 新頁面進入 dirty_pages（不是 snapshot！）
3. 掃描 dirty_pages_iter() → 不包含 [時間點 A] 新增的頁面
4. Young 物件可能被錯誤回收
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒才能可靠觸發
// 執行緒 1: 執行 minor GC (take_dirty_pages_snapshot)
// 執行緒 2: 在 snapshot 後建立 OLD→YOUNG 引用並觸發 add_to_dirty_pages

use rudo_gc::{Gc, GcCell, collect_full};

fn main() {
    // 1. 建立 old gen 物件
    let old_obj = Gc::new(GcCell::new(42));
    
    // 2. 執行 full GC 將物件 promote 到 old gen
    collect_full();
    
    // 3. 建立 young gen 物件
    let young_obj = Gc::new(GcCell::new(100));
    
    // 4. 在精確時序下：OLD → YOUNG 引用建立
    // 如果此時 add_to_dirty_pages 被呼叫但頁面未在 snapshot 中
    *old_obj.borrow_mut() = young_obj;
    
    // 5. 執行 minor GC
    // young_obj 可能被錯誤回收
    collect(); // minor only
    
    // 6. 存取 young_obj - UAF!
    println!("{}", *young_obj.borrow());
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**選項 1: 雙重緩衝 (Double Buffering)**
- 在掃描期間同時追蹤 `dirty_pages` 和 `dirty_pages_snapshot`
- 掃描完成後合併新頁面

**選項 2: 延遲清除標記**
- 不在 snapshot 時立即清除 `dirty_pages`
- 在 GC 週期結束後再清除

**選項 3: 混合掃描**
- 先掃描 snapshot，再檢查並掃描當前 dirty_pages
- 確保不遺漏任何 dirty page

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是典型的 snapshot 與即時追蹤之間的時間窗口問題。在 Chez Scheme 中，我們使用「卡片標記」(card marking) 來處理這個問題，確保在 snapshot 後的寫入也能被捕獲。對於頁面級追蹤，需要確保 dirty bit 的設置與頁面列表的更新是原子性的，或者採用雙重緩衝策略。

**Rustacean (Soundness 觀點):**
這是一個明確的記憶體安全問題。如果 young 物件被錯誤回收，任何後續存取都構成 UAF。建議在修復前標記為 `unsafe`，並在文件明確說明此時序要求。

**Geohot (Exploit 觀點):**
這個 bug 可以被利用來實現記憶體佈局控制攻擊。如果攻擊者能精確控制時序，可以:
1. 讓 victim 物件被錯誤回收
2. 重新分配相同記憶體
3. 建立 arbitrary write 原語

在多執行緒 WASM 環境下特別危險。

---

**Resolution:** Added `drain_dirty_pages_overflow()` to LocalHeap to capture pages added by write barriers after `take_dirty_pages_snapshot()`. All dirty-page scan sites now also iterate overflow: mark_minor_roots_multi, mark_minor_roots_parallel, mark_minor_roots (gc.rs), mark_slice and execute_final_mark (incremental.rs). Implements Option 3 (hybrid scan).
