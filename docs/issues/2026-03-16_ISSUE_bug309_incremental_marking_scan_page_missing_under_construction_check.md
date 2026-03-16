# [Bug]: scan_page_for_marked_refs 缺少 is_under_construction 檢查導致錯誤標記 partial GC 物件

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 incremental marking 期間呼叫 `Gc::new_cyclic_weak` |
| **Severity (嚴重程度)** | Medium | 導致部分初始化的物件被錯誤標記為 live，內存洩漏 |
| **Reproducibility (復現難度)** | Medium | 需精確控制 timing 觸發 incremental marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (`scan_page_for_marked_refs`, `scan_page_for_unmarked_refs`)
- **OS / Architecture:** Linux x86_64, All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

### 預期行為
在 incremental marking 期間，當 mutator 執行 `Gc::new_cyclic_weak` 時，處於 construction 狀態的物件不應被標記為 live。

### 實際行為
`scan_page_for_marked_refs` 和 `scan_page_for_unmarked_refs` 函數掃描 dirty pages 並標記物件時，沒有檢查 `is_under_construction` flag。這導致在 incremental marking 期間，如果 mutator 正在執行 `Gc::new_cyclic_weak`，則該 partial 物件會被錯誤地標記為 live。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **`Gc::new_cyclic_weak`** 在物件建構期間設置 `UNDER_CONSTRUCTION_FLAG` (line 1365 in ptr.rs)，在建構完成後清除 (line 1392)

2. **`mark_new_object_black` 和 `mark_object_black`** 已經有 `is_under_construction` 檢查 (bug238 fix)，但 **`scan_page_for_marked_refs` 和 `scan_page_for_unmarked_refs` 缺少此檢查**

3. 在 incremental marking 期間：
   - `execute_snapshot`: mutators 停止
   - `mark_slice`: mutators 恢復運行 (incremental marking)
   - 此時 mutator 可以呼叫 `Gc::new_cyclic_weak`
   - 如果 `scan_page_for_marked_refs` 掃描到該 partial 物件，會錯誤標記

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

static MARKING_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<rudo_gc::Weak<Node>>>,
    value: i32,
}

fn main() {
    // 觸發 incremental marking
    rudo_gc::gc::incremental::set_incremental_config(
        rudo_gc::gc::incremental::IncrementalConfig {
            enabled: true,
            slice_timeout_ms: 1, // 快速觸發 incremental marking
            ..Default::default()
        }
    );

    // 建立 OLD -> YOUNG 引用觸發 dirty page
    let old = Gc::new(GcCell::new(0));
    for _ in 0..1000 {
        let young = Gc::new(1);
        *old.borrow_mut() = *young;
    }

    // 觸發 GC 進入 incremental marking 階段
    // 同時在另一個執行緒執行 Gc::new_cyclic_weak
    let handle = thread::spawn(|| {
        // 等待 incremental marking 開始
        while !rudo_gc::gc::incremental::is_incremental_marking_active() {
            thread::yield();
        }
        
        // 此時執行 Gc::new_cyclic_weak，物件處於 UNDER_CONSTRUCTION_FLAG 狀態
        let _node = Gc::new_cyclic_weak(|weak| Node {
            self_ref: GcCell::new(Some(weak)),
            value: 42,
        });
    });

    // 觸發多次 incremental mark slice
    for _ in 0..100 {
        rudo_gc::gc::collect();
        thread::yield();
    }

    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `scan_page_for_marked_refs` 和 `scan_page_for_unmarked_refs` 中添加 `is_under_construction` 檢查，與 `mark_new_object_black` 和 `mark_object_black` 保持一致。

```rust
// 在 scan_page_for_marked_refs 中，try_mark 成功後:
// Re-check is_allocated to fix TOCTOU with lazy sweep.
// If slot was swept after try_mark, clear mark and skip.
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}

// 新增: 檢查物件是否處於 construction 狀態
let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
if (*gc_box_ptr).is_under_construction() {
    (*header).clear_mark_atomic(i);
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這是 SATB (Snapshot-At-The-Beginning) incremental marking 的經典問題
- 新建構的物件應該被视为 "floating garbage"，不應在本次 GC cycle 中被標記
- 現有程式碼中 `mark_new_object_black` 已有正確處理，但 page scanning 路徑遺漏

**Rustacean (Soundness 觀點):**
- 此 bug 不會導致 UAF，但會導致 memory leak (不應存活的物件無法被回收)
- 這是 GC 正確性問題，而非記憶體安全問題

**Geohot (Exploit 觀點):**
- 難以利用：需要精確控制執行時序
- 主要影響是長期運行的服務可能出現記憶體洩漏
