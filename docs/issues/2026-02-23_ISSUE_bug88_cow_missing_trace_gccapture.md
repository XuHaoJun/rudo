# [Bug]: std::borrow::Cow 缺少 Trace 與 GcCapture 實作導致無法與 Gc 整合

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能使用 Cow 作為 Clone-on-Write 優化模式與 GC 指標整合 |
| **Severity (嚴重程度)** | High | 導致 Cow<Gc<T>> 無法編譯，無法使用 Clone-on-Write 模式 |
| **Reproducibility (復現難度)** | Low | 直接嘗試使用 Cow<Gc<T>> 即可發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Trace` trait implementation in `trace.rs`, `GcCapture` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`std::borrow::Cow` 同時缺少 `Trace` 與 `GcCapture` trait 實作。`Cow` 是常見的 Clone-on-Write 優化模式，應該能夠與 GC 指標整合。

### 預期行為
- `Cow<Gc<T>>` 應該可以編譯並正常運作
- GC 應該能夠正確追蹤 Cow 內部的指標

### 實際行為
- `Cow<Gc<T>>` 無法編譯，因為 Cow 沒有實作 Trace
- 這與其他標準庫類型（如 Vec, Box, Rc）不同

### 相關已回報問題
- bug79: VecDeque 與 LinkedList 缺少 GcCapture
- bug82: BinaryHeap 缺少 GcCapture
- bug87: BinaryHeap 缺少 Trace

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `trace.rs` 的 imports 中，沒有包含 `std::borrow::Cow`：

```rust
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, LinkedList, VecDeque};
// 沒有包含 Cow
```

也沒有對應的 `unsafe impl Trace for Cow<T>`。

對比：
- `Vec<T>`: 有 Trace (trace.rs:313) + GcCapture (cell.rs:418)
- `Box<T>`: 有 Trace (trace.rs:288) + GcCapture (cell.rs:512)
- `Rc<T>`: 有 Trace (trace.rs:296) + GcCapture (cell.rs:536)
- `Cow<T>`: **無** Trace + **無** GcCapture

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::borrow::Cow;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 嘗試建立 Cow 儲存 Gc 指標 - 編譯錯誤！
    let gc = Gc::new(Data { value: 42 });
    let cow: Cow<Gc<Data>> = Cow::Borrowed(&gc);
}
```

編譯錯誤：
```
error[E0277]: the trait bound `Cow<Gc<Data>>: Trace` is not satisfied
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 在 `trace.rs` 新增 Cow Trace 實作：
```rust
use std::borrow::Cow;

unsafe impl<T: Trace> Trace for Cow<'_, T> {
    fn trace(&self, visitor: &mut impl Visitor) {
        match self {
            Cow::Borrowed(t) => t.trace(visitor),
            Cow::Owned(t) => t.trace(visitor),
        }
    }
}
```

2. 在 `cell.rs` 新增 Cow GcCapture 實作：
```rust
use std::borrow::Cow;

impl<T: GcCapture + 'static> GcCapture for Cow<'_, T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        match self {
            Cow::Borrowed(t) => t.capture_gc_ptrs(),
            Cow::Owned(t) => t.capture_gc_ptrs(),
        }
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        match self {
            Cow::Borrowed(t) => t.capture_gc_ptrs_into(ptrs),
            Cow::Owned(t) => t.capture_gc_ptrs_into(ptrs),
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Cow 是常見的優化模式，在 Scheme 實現中也常用類似模式延遲複製。缺少 Trace 會阻止這種常見模式的使用。

**Rustacean (Soundness 觀點):**
這是基本的 API 完整性問題。標準庫提供的常用類型應該都能與 Gc 整合。

**Geohot (Exploit 攻擊觀點):**
缺少 Trace 會直接導致無法使用，是編譯期錯誤而非執行期問題。

---

## 關聯 Issue

- bug79: VecDeque 與 LinkedList 缺少 GcCapture
- bug82: BinaryHeap 缺少 GcCapture
- bug87: BinaryHeap 缺少 Trace

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

1. Added `Trace` impl for `Cow<'_, B>` in `trace.rs` (traces both Borrowed and Owned variants)
2. Added `GcCapture` impl for `Cow<'_, B>` in `cell.rs` (iterates elements, delegates to `capture_gc_ptrs_into`)
3. Added blanket `GcCapture` impl for `&T` where `T: GcCapture` (required for Cow::Borrowed)
4. Added regression test `tests/bug88_cow_missing_trace_gccapture.rs`
