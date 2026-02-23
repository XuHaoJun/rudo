# [Bug]: BinaryHeap 缺少 GcCapture 實作導致指標遺漏

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能使用 BinaryHeap 作為優先級隊列 |
| **Severity (嚴重程度)** | High | SATB barrier 失效導致年輕物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需特定使用模式（minor GC + 跨容器引用） |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` trait implementation for std collections in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`std::collections::BinaryHeap` 缺少 `GcCapture` trait 實作。當此容器包含 `Gc<T>` 指標時，SATB (Snapshot-At-The-Beginning) write barrier 無法正確擷取內部指標，導致標記階段可能遺漏這些指標，造成記憶體回收錯誤。

### 預期行為 (Expected Behavior)
- `BinaryHeap<Gc<T>>` 應該實作 `GcCapture`，使 write barrier 能正確追蹤容器內的 GC 指標

### 實際行為 (Actual Behavior)
- 缺少 `GcCapture` 實作，導致 write barrier 的 `capture_gc_ptrs_into` 無法遍歷容器內的元素
- SATB barrier 無法記錄 OLD→YOUNG 引用，年輕物件可能在 minor GC 時被錯誤回收

### 相關已回報問題
- bug79: VecDeque 與 LinkedList 缺少 GcCapture（本日回報）
- 本 issue 補充 BinaryHeap 的缺失

---

## 🔬 根本原因分析 (Root Cause Analysis)

`GcCapture` trait 需要實作 `capture_gc_ptrs_into` 方法來遍歷類型內部的所有 GC 指標。`Vec<T>`、`HashMap<K, V>`、`BTreeMap<K, V>`、`HashSet<T>`、`BTreeSet<T>` 等容器都已有實作，但 `BinaryHeap<T>` 缺少實作。

在 cell.rs 中可見現有實作（第 446 行 imports）：
- `BTreeMap<K, V>`: 第 467 行
- `BTreeSet<T>`: 第 498 行
- `HashMap<K, V, S>`: 第 448 行（但 cell.rs 沒 import，需確認）
- `HashSet<T, S>`: 第 484 行

但 BinaryHeap 沒有對應實作。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full, GcCell};
use std::collections::BinaryHeap;
use std::cmp::Reverse;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 建立 OLD 物件
    let old = Gc::new(GcCell::new(BinaryHeap::new()));
    
    // promote to old generation
    collect_full();
    
    {
        // 建立年輕 Gc 並加入 BinaryHeap
        let young = Gc::new(Data { value: 42 });
        old.borrow_mut().push(Reverse(young));
    }
    
    // 呼叫 minor GC - 若 BinaryHeap 缺少 GcCapture，
    // 年輕物件可能因未被標記而被錯誤回收
    // 使用 collect() 而非 collect_full() 來觸發 minor GC
    rudo_gc::collect();
    
    // 嘗試存取 - 如果 BinaryHeap 有正確的 GcCapture，應該可以存取
    if let Some(gc) = old.borrow().peek() {
        println!("Value: {}", gc.0.value); // 可能 panic 或印出未定義記憶體
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `crates/rudo-gc/src/cell.rs` 中新增以下實作：

```rust
use std::collections::BinaryHeap;

impl<T: GcCapture + 'static> GcCapture for BinaryHeap<T> {
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
BinaryHeap 是常見的資料結構（優先級隊列），與 Vec、HashMap 等容器相同，都需要在 SATB barrier 中被遍歷。缺少此實作會導致容器內的 GC 指標在標記階段被遺漏。

**Rustacean (Soundness 觀點):**
這不是 UB，但會導致記憶體回收錯誤（年輕物件被錯誤回收）。類型本身 sound，但執行時會有邏輯錯誤。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用此行為進行記憶體腐蝕。但此 bug 導致物件被回收而非保留，更像是拒絕服務而非任意讀寫。

---

## 關聯 Issue

- bug79: VecDeque 與 LinkedList 缺少 GcCapture（本 issue 的相關 issue）
