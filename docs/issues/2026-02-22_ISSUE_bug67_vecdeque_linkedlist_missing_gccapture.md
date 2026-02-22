# [Bug]: VecDeque 與 LinkedList 缺少 GcCapture 實作導致指標遺漏

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 VecDeque 或 LinkedList 中包含 Gc<T> 指針 |
| **Severity (嚴重程度)** | High | 導致 GC 無法追蹤指標，可能造成記憶體洩露或 use-after-free |
| **Reproducibility (復現難度)** | Medium | PoC 相對簡單，但需確認 Gc<T> 在容器內部 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** std::collections::VecDeque, std::collections::LinkedList
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`std::collections::VecDeque<T>` 與 `std::collections::LinkedList<T>` 缺少 `GcCapture` trait 實作。

當 `Gc<T>` 指針存於 `VecDeque<T>` 或 `LinkedList<T>` 內部時，GC 將無法正確追蹤這些指標，導致：
1. 指標可能被錯誤回收
2. 標記階段可能遺漏這些指標

### 預期行為
`GcCapture` 應該能夠從 `VecDeque<T>` 與 `LinkedList<T>` 內部提取 GC 指針。

### 實際行為
沒有 `GcCapture` 實作，GC 在追蹤時會遺漏這些類型內部的指標。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs` 中，`Vec<T>` 已有 `GcCapture` 實作（line 418-430），但 `VecDeque<T>` 與 `LinkedList<T>` 卻沒有。

現有實作模式（cell.rs:418-430）：
```rust
impl<T: GcCapture + 'static> GcCapture for Vec<T> {
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

`trace.rs` 中已有 `Trace` 實作：
- VecDeque: line 394-411
- LinkedList: line 414-421

但 `cell.rs` 中缺少對應的 `GcCapture` 實作。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell};
use std::collections::{VecDeque, LinkedList};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let deque = VecDeque::new();
    // 模擬 GC 追蹤 - 這會失敗因為 GcCapture 未實作
    let ptrs = Vec::new();
    // deque.capture_gc_ptrs_into(&mut ptrs); // 編譯錯誤！

    let list = LinkedList::new();
    // list.capture_gc_ptrs_into(&mut ptrs); // 編譯錯誤！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cell.rs` 中新增：

```rust
use std::collections::{VecDeque, LinkedList};

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
VecDeque 和 LinkedList 是常見的資料結構，缺少 GcCapture 會導致 GC 無法正確追蹤指標。這與 Vec 的問題相同（已有 GcCapture），但 VecDeque 和 LinkedList 使用不同的內部結構。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果 GC 無法追蹤指標，包含 Gc<T> 的容器可能導致 use-after-free 或記憶體洩露。

**Geohot (Exploit 觀點):**
攻擊者可能利用此漏洞，通過控制何時 GC 運行來觸發 use-after-free。
