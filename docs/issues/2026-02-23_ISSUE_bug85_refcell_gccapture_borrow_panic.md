# [Bug]: RefCell GcCapture 使用 borrow() 可能導致 panic

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當程式碼在 GC 期間持有 RefCell 的可變借用時會觸發 |
| **Severity (嚴重程度)** | High | 會導致程式 panic，可能導致程式崩潰 |
| **Reproducibility (復現難度)** | Medium | 需要在 GC 期間保持 RefCell 的可變借用 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture for RefCell<T>` in `cell.rs:583-594`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`RefCell<T>` 的 `GcCapture` 實作應該在無法獲取不可變借用時優雅地跳過擷取，而不是 panic。

### 實際行為 (Actual Behavior)
在 `cell.rs:590-593` 中，`GcCapture for RefCell<T>` 使用 `self.borrow()`，這會在有活躍的可變借用時 panic：

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let value = self.borrow();  // 如果有活躍的可變借用，這會 panic!
    value.capture_gc_ptrs_into(ptrs);
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 incremental marking 期間，GC 會追蹤可達物件並呼叫 `capture_gc_ptrs_into`。如果此時有執行緒持有 `RefCell` 的可變借用，呼叫 `RefCell::borrow()` 會 panic 而不是跳過。

注意：程式碼中的註解（cell.rs:792-793）說「If T is borrowed mutably (RefCell), capture_gc_ptrs_into will skip capturing」，但實際實作會 panic，與註解不一致。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};
use std::cell::RefCell;

#[derive(Trace)]
struct Data {
    cell: RefCell<Vec<Gc<i32>>>,
}

fn main() {
    // 建立包含 RefCell<Vec<Gc<T>>> 的 Gc
    let gc = Gc::new(Data {
        cell: RefCell::new(vec![Gc::new(42)]),
    });

    // 取得可變借用
    let mut borrow = gc.cell.borrow_mut();
    
    // 在保持可變借用的情況下觸發 GC
    // 這會導致 RefCell::borrow() panic
    collect_full();
    
    // 即使 drop borrow 後也會有機會觸發 GC
    // 因為 GC 可能發生在 borrow() 和 之間
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `cell.rs:590-593` 修改為使用 `try_borrow()` 而不是 `borrow()`：

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Some(value) = self.try_borrow().ok() {
        value.capture_gc_ptrs_into(ptrs);
    }
    // 如果無法獲取借用，優雅地跳過
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 追蹤期間，應該避免任何可能導致 panic 的操作。使用 `try_borrow()` 允許在無法安全獲取借用時跳過，這與其他 GC 實現中的模式一致。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 UB，但會導致程式在 GC 期間崩潰，這是不可接受的。GC 應該是可靠的，不應該因為用戶程式持有借用而崩潰。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過故意保持 RefCell 的可變借用來觸發 GC，從而導致程式崩潰，這可以被用作拒絕服務攻擊向量。

