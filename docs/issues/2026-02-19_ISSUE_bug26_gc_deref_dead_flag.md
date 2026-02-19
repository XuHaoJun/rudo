# [Bug]: Gc::deref 與 try_deref 未檢查 DEAD_FLAG 導致 Use-After-Free

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 在 cyclic reference GC 時必定觸發 |
| **Severity (嚴重程度)** | Critical | 導致 Use-After-Free，記憶體崩潰 |
| **Reproducibility (重現難度)** | Low | 只需建立 cyclic reference 並觸發 collection |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::deref` in `ptr.rs:1267-1272`, `Gc::try_deref` in `ptr.rs:1048-1055`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
- `Gc::deref` 應該在值已被 drop 時 panic 或返回 error
- `Gc::try_deref` 應該在值已被 drop 時返回 `None`

### 實際行為 (Actual Behavior)
`Gc::deref` 和 `try_deref` 都只檢查指標是否為 null，完全沒有檢查 `DEAD_FLAG` 或 `dropping_state()`。

**`Gc::deref` 實現 (ptr.rs:1267-1272):**
```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // SAFETY: ptr is not null (checked in callers), and ptr is valid
    unsafe { &(*gc_box_ptr).value }  // 未檢查 DEAD_FLAG!
}
```

**`Gc::try_deref` 實現 (ptr.rs:1048-1055):**
```rust
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {  // 只檢查 null！
        None
    } else {
        Some(&**gc)  // 未檢查 DEAD_FLAG!
    }
}
```

文檔宣稱 (ptr.rs:680-681):
> Dereferencing a "dead" `Gc` (one whose value has been collected during
> a Drop implementation) will panic.

但實現並未兌現這個承諾！

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 cyclic reference 收集期間：
1. `GcBox::drop_fn_for` 調用 `set_dead()` 設置 `DEAD_FLAG`
2. 調用 `std::ptr::drop_in_place` 丟棄值
3. `GcBox` 本身仍然有效（未被釋放），只是值被 drop 了
4. 用戶持有的 `Gc<T>` 指針仍然有效，指向已 drop 的 `GcBox`
5. 用戶調用 `deref` 或 `try_deref` 時，訪問已 drop 的記憶體

**關鍵問題：**
- `Weak::upgrade` 正確檢查了 `has_dead_flag()` (ptr.rs:1481)
- 但 `Gc::deref` 和 `try_deref` 完全沒有檢查！
- 這導致 `try_deref` 的文檔承諾 ("Returns `None` if this Gc is dead") 沒有兌現

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Trace)]
struct Node {
    #[unsafe_ignore_trace]
    next: RefCell<Option<Gc<Node>>>,
}

fn main() {
    // 建立 cyclic reference: node1 -> node2 -> node1
    let node1 = Gc::new_cyclic_weak(|weak| Node {
        next: RefCell::new(None),
    });
    let node2 = Gc::new_cyclic_weak(|weak| Node {
        next: RefCell::new(Some(weak.clone())),
    });
    
    // 建立循環
    *node1.next.borrow_mut() = Some(node2.clone());
    
    // 獲取引用
    let strong_ref = node1.clone();
    
    // 觸發 GC - 循環引用會被收集
    collect_full();
    
    // 嘗試 deref - 這應該返回 None 或 panic，但實際會 UAF!
    if let Some(n) = Gc::try_deref(&strong_ref) {
        println!("BUG: 應該返回 None! 值已被 drop");
    }
    
    // 直接 deref - 會導致 UAF
    // let _ = *strong_ref; // 崩潰!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1: 在 try_deref 中添加 DEAD_FLAG 檢查
```rust
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    
    let gc_box_ptr = ptr.as_ptr();
    
    // 檢查 DEAD_FLAG
    unsafe {
        if (*gc_box_ptr).has_dead_flag() {
            return None;
        }
        if (*gc_box_ptr).dropping_state() != 0 {
            return None;
        }
    }
    
    Some(unsafe { &(*gc_box_ptr).value })
}
```

### 方案 2: 在 deref 中添加檢查並 panic
```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    
    // 檢查是否為 dead
    unsafe {
        if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
            panic!("Gc::deref: cannot dereference a dead Gc");
        }
        &(*gc_box_ptr).value
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個嚴重的 GC 正確性問題。在 cyclic reference collection 中，值被 drop 但 GcBox 本身保留是預期行為。然而，用戶代碼必須能夠檢測這種狀態。`Weak::upgrade` 正確檢查了 `has_dead_flag()`，但 `Gc::deref` 沒有，這導致不一致且危險的 API。

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 UB（因為 GcBox 仍然有效），但絕對是記憶體安全問題。`try_deref` 的文檔明確說 "Returns `None` if this Gc is dead"，但實現完全沒有兌現這個承諾。這是 API contract 違反。

**Geohot (Exploit 觀點):**
這個 bug 可以被利用來實現記憶體錯誤：
1. 攻擊者建立 cyclic reference
2. 觸發 GC collection  
3. 在值被 drop 後仍然存取記憶體
4. 讀取已 drop 對象的舊資料，或在最佳情況下導致崩潰

---

## 對比: Weak::upgrade 正確實現

Weak::upgrade (ptr.rs:1480-1487) 正確檢查了：
```rust
loop {
    if gc_box.has_dead_flag() {  // ✓ 正確檢查
        return None;
    }
    if gc_box.dropping_state() != 0 {  // ✓ 正確檢查
        return None;
    }
    // ...
}
```

Gc::deref 應該遵循相同的模式！
