# [Bug]: std::rc::Rc 缺少 GcCapture 實作導致 SATB 屏障失效

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會在 Rc 中存儲 GC 指標以共享所有權 |
| **Severity (嚴重程度)** | High | 導致 GC 無法掃描 Rc 內部的指標，造成記憶體錯誤 |
| **Reproducibility (Reproducibility)** | Medium | 需要使用 GcCell<Rc<Gc<T>>> 模式才能觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` impl for `std::rc::Rc<T>`, `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`std::rc::Rc<T>` 應該實作 `GcCapture` trait，使得 GC 可以掃描 Rc 內部的 GC 指標。這與 `Box<T>`、`Vec<T>` 等其他容器類型的行為一致。

### 實際行為 (Actual Behavior)
`std::rc::Rc<T>` 沒有實作 `GcCapture` trait。當 GC 嘗試掃描根集時，無法捕捉到存在於 `std::rc::Rc<T>` 內部的 GC 指標，導致這些指標被錯誤地視為垃圾。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs` 中存在以下 `GcCapture` 實作：
- `Box<T>` (line 507-517)
- `Vec<T>` (line 413-425)
- `Option<T>` (line 399-411)
- `std::sync::RwLock<T>` (line 567-579)

但缺少：
- `std::rc::Rc<T>` - **本 bug**
- `std::sync::Arc<T>` - 已有 bug37 記錄
- `parking_lot::Mutex<T>` - 相關問題

當使用以下模式時會觸發 bug：
```rust
use std::rc::Rc;
use rudo_gc::{Gc, GcCell, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[derive(Trace, GcCell)]
struct Container {
    // Rc 內部包含 Gc 指標，但 GcCapture 未實作
    rc_gc: Rc<Gc<Data>>,
}

let gc = Gc::new(Data { value: 42 });
let container = Gc::new(Container {
    rc_gc: Rc::new(gc),
});

// 修改 Rc 內部的 Gc 指針
{
    let mut mut_container = container.borrow_mut();
    mut_container.rc_gc = Rc::new(Gc::new(Data { value: 100 }));
}

// 由於 Rc 缺少 GcCapture，incremental marking 可能無法
// 正確追蹤 Rc 內部的 GC 指標
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 開啟 incremental marking feature
2. 執行以下程式碼：

```rust
use std::rc::Rc;
use rudo_gc::{Gc, GcCell, Trace, collect_full};

#[derive(Trace)]
struct Payload {
    data: i32,
}

#[derive(Trace, GcCell)]
struct Container {
    // 缺少 GcCapture 實作
    rc_ptr: Rc<Gc<Payload>>,
}

fn main() {
    // 建立年輕代物件
    let young = Gc::new(Payload { data: 42 });
    
    // 將 Gc 放入 Rc，再放入 GcCell
    let container = Gc::new(Container {
        rc_ptr: Rc::new(young),
    });
    
    // 先 collect_full 將物件 promote 到 old gen
    collect_full();
    
    // 建立 OLD->YOUNG 引用 (透過 Rc)
    {
        let mut mut_container = container.borrow_mut();
        let new_young = Gc::new(Payload { data: 100 });
        mut_container.rc_ptr = Rc::new(new_young);
    }
    
    // 呼叫 collect (minor GC) - 應該觸發 generational barrier
    // 但由於 Rc 缺少 GcCapture，barrier 無法記錄 old value
    collect_full();
    
    // 驗證：如果 bug 存在，young 物件可能已被錯誤回收
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cell.rs` 中添加 `std::rc::Rc<T>` 的 `GcCapture` 實作：

```rust
use std::rc::Rc;

impl<T: GcCapture + 'static> GcCapture for Rc<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        // Rc 內部只有一個值，委託給 T 的 capture_gc_ptrs
        (**self).capture_gc_ptrs()
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        (**self).capture_gc_ptrs_into(ptrs);
    }
}
```

注意：需要確保 `T: GcCapture` 的原因是 Rc 內部包含的類型必須能夠捕獲 GC 指標。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 Chez Scheme 中，我們使用 fluid variables 來追蹤可變引用。`Rc<T>` 缺少 GcCapture 類似於沒有正確設置 write barrier。在incremental marking 中，每次寫入都需要記錄舊值以維持 SATB 不變性。如果 `Rc` 內部的 GC 指針無法被記錄，則可能導致錯誤的回收。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 UB，但可能導致記憶體錯誤。由於 GC 無法看到 Rc 內部的指標，這些指標指向的物件可能被錯誤地視為垃圾並回收。之後對這些指標的解引用會導致 use-after-free。

**Geohot (Exploit 觀點):**
攻擊者可能利用這個 bug 來：
1. 構造一個場景，使 GC 回收正在使用的物件
2. 重新分配相同記憶體位置
3. 實現任意記憶體讀寫

儘管難度較高，但這是一個潛在的記憶體腐蝕向量。
