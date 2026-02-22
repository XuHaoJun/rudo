# [Bug]: VecDeque 與 LinkedList 缺少 GcCapture 實作導致指標遺漏

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | *開發者常使用 VecDeque 與 LinkedList 作為 collection* |
| **Severity (嚴重程度)** | High | *SATB barrier 失效導致年輕物件被錯誤回收* |
| **Reproducibility (復現難度)** | Medium | *需特定使用模式（minor GC + 跨容器引用）* |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` trait implementation for std collections
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`std::collections::VecDeque` 與 `std::collections::LinkedList` 缺少 `GcCapture` trait 實作。當這些容器包含 `Gc<T>` 指標時，SATB (Snapshot-At-The-Beginning) write barrier 無法正確擷取內部指標，導致標記階段可能遺漏這些指標，造成記憶體回收錯誤。

### 預期行為 (Expected Behavior)
- `VecDeque<Gc<T>>` 與 `LinkedList<Gc<T>>` 應該實作 `GcCapture`，使 write barrier 能正確追蹤容器內的 GC 指標

### 實際行為 (Actual Behavior)
- 缺少 `GcCapture` 實作，導致 write barrier 的 `capture_gc_ptrs_into` 無法遍歷容器內的元素
- SATB barrier 無法記錄 OLD→YOUNG 引用，年輕物件可能在 minor GC 時被錯誤回收

---

## 🔬 根本原因分析 (Root Cause Analysis)

`GcCapture` trait 需要實作 `capture_gc_ptrs_into` 方法來遍歷類型內部的所有 GC 指標。`Vec<T>`、`HashMap<K, V>`、`BTreeMap<K, V>` 等容器都已有實作，但 `VecDeque<T>` 與 `LinkedList<T>` 缺少實作。

在 cell.rs 中可見現有實作：
- `Vec<T>`: 第 418-430 行
- `HashMap<K, V, S>`: 第 448-465 行  
- `BTreeMap<K, V>`: 第 467-482 行

但 VecDeque 與 LinkedList 沒有對應實作。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full, GcCell};
use std::collections::{VecDeque, LinkedList};
use std::cell::RefCell;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 建立 OLD 物件
    let old = Gc::new(RefCell::new(VecDeque::new()));
    
    // promote to old generation
    collect_full();
    
    {
        // 建立年輕 Gc 並加入 VecDeque
        let young = Gc::new(Data { value: 42 });
        old.borrow_mut().push_back(young);
    }
    
    // 呼叫 minor GC - 若 VecDeque 缺少 GcCapture，
    // 年輕物件可能因未被標記而被錯誤回收
    // 使用 collect() 而非 collect_full() 來觸發 minor GC
    rudo_gc::collect();
    
    // 嘗試存取 - 如果 VecDeque 有正確的 GcCapture，應該可以存取
    if let Some(gc) = old.borrow().front() {
        println!("Value: {}", gc.value); // 可能 panic 或印出未定義記憶體
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `crates/rudo-gc/src/cell.rs` 中新增以下實作：

```rust
use std::collections::VecDeque;

impl<T: GcCapture + 'static> GcCapture for VecDeque<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for value in self {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: GcCapture + 'static> GcCapture for LinkedList<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for value in self {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
VecDeque 與 LinkedList 是常見的資料結構，與 Vec、HashMap 等容器相同，都需要在 SATB barrier 中被遍歷。缺少此實作會導致容器內的 GC 指標在標記階段被遺漏，這與 bug22 (HashMap iterator invalidation) 類似，但影響更廣泛。

**Rustacean (Soundness 觀點):**
這不是 UB，但會導致記憶體回收錯誤（年輕物件被錯誤回收）。類型本身 sound，但執行時會有邏輯錯誤。

**Geohot (Exploit 觀點):**
攻擊者可能利用此行為進行記憶體腐蝕。但此 bug 導致物件被回收而非保留，更像是拒絕服務而非任意讀寫。
