# [Bug]: Final mark may fail to trace through objects under construction

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 僅在 GC 在 `Gc::new_cyclic_weak` 的 `data_fn` 期間執行時發生 |
| **Severity (嚴重程度)** | `High` | 可能導致 UAF 或过早回收 |
| **Reproducibility (復現難度)** | `Low` | 需要精確的時序配合 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `incremental marking` (`gc/incremental.rs`)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

在 `execute_final_mark` 期間，如果物件處於「建構中」狀態，`trace_and_mark_object` 會跳過追蹤其子節點。

### 預期行為 (Expected Behavior)
在 final mark 階段，應該追蹤所有可達物件的子節點，確保所有可達物件都被標記。

### 實際行為 (Actual Behavior)
在 `trace_and_mark_object` 中（`gc/incremental.rs:758-759`）：
```rust
if (*gc_box.as_ptr()).is_under_construction() {
    return;
}
```
物件被標記但其子節點不被追蹤。如果物件的子節點只能通過這個物件到達，則子節點不會被標記。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `gc/incremental.rs` 的 `trace_and_mark_object` 函數：

```rust
unsafe fn trace_and_mark_object(gc_box: NonNull<GcBox<()>>, state: &IncrementalMarkState) {
    // ... validation checks ...
    
    if (*gc_box.as_ptr()).is_under_construction() {
        return;  // 提早返回，不追蹤子節點
    }
    
    // ... 繼續追蹤子節點 ...
}
```

當物件處於建構中狀態時，函數提前返回。物件本身被標記（第 537 行），但其子節點不會被添加到工作清單。

這在以下情境中有問題：
1. `Gc::new_cyclic_weak` 正在執行中
2. 使用者 `data_fn` 建立了循環引用
3. GC 在此期間執行了 final mark
4. 某些物件被標記為可達但其子節點未被追蹤

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell};
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Trace)]
struct CyclicNode {
    value: GcCell<Option<Gc<CyclicNode>>>,
}

fn main() {
    // 建立循環引用，data_fn 期間會有物件處於建構中
    let weak_ref = Gc::new_cyclic(|weak_self| {
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        
        // 在 data_fn 執行期間，嘗試觸發 GC
        // 如果 final mark 在此時執行，
        // is_under_construction() 的物件會被跳過
        if count == 0 {
            rudo_gc::collect_full();
        }
        
        CyclicNode {
            value: GcCell::new(Some(weak_self.clone())),
        }
    });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：移除 `trace_and_mark_object` 中的 `is_under_construction()` 檢查
- 在 final mark 期間，所有執行緒都已停止，不應有在建構中的物件
- 如果有，應該是一個錯誤，而不是靜默跳過

選項 2：在 `execute_final_mark` 之前驗證沒有在建構中的物件
- 在開始 final mark 之前，新增一個階段來清理任何在建構中的物件

選項 3：追蹤在建構中的物件並在之後重新處理
- 將這些物件添加到一個特殊清單，完成建構後重新追蹤

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 final mark 階段，所有 mutator 執行緒都應該在 safepoint。此時不應有任何物件處於在建構中狀態。如果 `is_under_construction()` 返回 true，這表示發生了不正常的時序問題。建議在 final mark 開始前新增一個驗證階段。

**Rustacean (Soundness 觀點):**
`is_under_construction()` 檢查是為了防止在物件部分初始化時追蹤其指標。但在 final mark 期間，所有執行緒都已停止，不應有這種情況。這個檢查可能過度防御，導致在邊緣情況下遺漏標記。

**Geohot (Exploit 觀點):**
如果攻擊者能夠在 `new_cyclic` 的 `data_fn` 期間觸發 GC，可能會導致物件被錯誤標記但其子節點未被追蹤。這可能導致 Use-After-Free 或過早回收。這個檢查原本是為了安全性，但在 final mark 期間不應該發生。
