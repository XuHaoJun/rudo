# [Bug]: BinaryHeap 缺少 Trace 與 GcCapture 實作導致無法與 Gc 整合

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能使用 BinaryHeap 作為優先級隊列儲存 GC 指標 |
| **Severity (嚴重程度)** | Critical | 導致 BinaryHeap<Gc<T>> 無法編譯，無法使用 |
| **Reproducibility (復現難度)** | Low | 直接嘗試使用 BinaryHeap<Gc<T>> 即可發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Trace` trait implementation for std collections in `trace.rs`, `GcCapture` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`std::collections::BinaryHeap` 同時缺少 `Trace` 與 `GcCapture` trait 實作。

### 預期行為
- `BinaryHeap<Gc<T>>` 應該可以編譯並正常運作
- GC 應該能夠正確追蹤 BinaryHeap 內的指標

### 實際行為
- `BinaryHeap<Gc<T>>` 無法編譯，因為 BinaryHeap 沒有實作 Trace
- 這與 VecDeque、LinkedList 不同，後者至少有 Trace 實作（僅缺少 GcCapture）

### 相關已回報問題
- bug79: VecDeque 與 LinkedList 缺少 GcCapture
- bug82: BinaryHeap 缺少 GcCapture（但未發現 Trace 也缺少）

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `trace.rs:7` 的 imports 中：
```rust
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, LinkedList, VecDeque};
```

可以看到 BinaryHeap 並未被 import，也沒有對應的 `unsafe impl Trace for BinaryHeap<T>`。

對比：
- `Vec<T>`: 有 Trace (trace.rs:313) + GcCapture (cell.rs:418)
- `VecDeque<T>`: 有 Trace (trace.rs:394) + **無** GcCapture
- `LinkedList<T>`: 有 Trace (trace.rs:414) + **無** GcCapture
- `BinaryHeap<T>`: **無** Trace + **無** GcCapture

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::collections::BinaryHeap;
use std::cmp::Reverse;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 嘗試建立 BinaryHeap 儲存 Gc 指標 - 編譯錯誤！
    let heap: BinaryHeap<Reverse<Gc<Data>>> = BinaryHeap::new();
    heap.push(Reverse(Gc::new(Data { value: 42 })));
}
```

編譯錯誤：
```
error[E0277]: the trait bound `BinaryHeap<Reverse<Gc<Data>>>: Trace` is not satisfied
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 在 `trace.rs` 新增 BinaryHeap Trace 實作：
```rust
use std::collections::BinaryHeap;

unsafe impl<T: Trace> Trace for BinaryHeap<T> {
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
        }
        // Mark the BinaryHeap's storage buffer page as dirty
        if !self.is_empty() {
            unsafe {
                // BinaryHeap stores elements in a Vec-like internal buffer
                crate::heap::mark_page_dirty_for_ptr(self.as_slice().as_ptr() as *const u8);
            }
        }
    }
}
```

2. 在 `cell.rs` 新增 BinaryHeap GcCapture 實作：
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
BinaryHeap 是標準庫提供的優先級隊列實現，與 VecDeque、LinkedList 一樣是常見資料結構。缺少 Trace 實作是最基本的問題，必須先解決才能談論 GcCapture。

**Rustacean (Soundness 觀點):**
這是基本的 API 完整性問題。標準庫提供的 collection 應該都能與 Gc 整合。

**Geohot (Exploit 攻擊觀點):**
缺少 Trace 會直接導致無法使用，是編譯期錯誤而非執行期問題。

---

## 關聯 Issue

- bug79: VecDeque 與 LinkedList 缺少 GcCapture
- bug82: BinaryHeap 缺少 GcCapture（本 issue 補充：根本原因是缺少 Trace）
