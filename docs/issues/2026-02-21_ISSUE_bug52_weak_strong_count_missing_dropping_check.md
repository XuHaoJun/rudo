# [Bug]: Weak::strong_count() 與 Weak::weak_count() 缺少 dropping_state 檢查

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 object 被 drop 時同時呼叫這些函數 |
| **Severity (嚴重程度)** | Medium | 可能讀取到不一致的計數值或在邊界情況下出現問題 |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序條件才能觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>::strong_count()`, `Weak<T>::weak_count()`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為
`Weak::strong_count()` 和 `Weak::weak_count()` 應該在物件正在被 drop 時（`dropping_state() != 0`）返回安全的值或進行適當的檢查。

### 實際行為

**`Weak::strong_count()` (ptr.rs:1690-1707):**
```rust
pub fn strong_count(&self) -> usize {
    // ... 檢查 null 和 alignment ...
    
    unsafe {
        if (*ptr.as_ptr()).has_dead_flag() {
            0
        } else {
            (*ptr.as_ptr()).ref_count().get()  // 沒有檢查 dropping_state()!
        }
    }
}
```

**`Weak::weak_count()` (ptr.rs:1711-1717):**
```rust
pub fn weak_count(&self) -> usize {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return 0;
    };

    unsafe { (*ptr.as_ptr()).weak_count() }  // 沒有任何檢查!
}
```

兩個函數都缺少對 `dropping_state()` 的檢查。這與 bug49 涵蓋的 `Gc::ref_count()` 和 `Gc::weak_count()` 是相同的模式，但影響不同的類型。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/ptr.rs:1690-1717`

對比 `Gc::ref_count()` (bug49) 和 `Weak::strong_count()`：
- 兩者都只檢查 `has_dead_flag()`，都忽略 `dropping_state()`

對比 `Gc::weak_count()` (bug49) 和 `Weak::weak_count()`：
- `Gc::weak_count()` 有基本的載入檢查
- `Weak::weak_count()` 完全沒有任何有效性檢查

當物件處於 dropping 狀態時（`dropping_state >= 1`），訪問 `ref_count` 或 `weak_count` 可能會讀取到正在變化的值，導致不一致的結果。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // 在一個執行緒中 drop 並觸發 GC
    let gc_clone = gc.clone();
    thread::spawn(move || {
        drop(gc_clone);
        collect_full();
    }).join().unwrap();
    
    // 另一個執行緒同時呼叫 weak_count
    // dropping_state 可能 != 0，導致讀取到不一致的值
    let _ = weak.strong_count();
    let _ = weak.weak_count();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

為 `Weak::strong_count()` 添加 `dropping_state()` 檢查：

```rust
pub fn strong_count(&self) -> usize {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return 0;
    };
    let ptr_addr = ptr.as_ptr() as usize;
    let alignment = std::mem::align_of::<GcBox<T>>();
    if ptr_addr % alignment != 0 {
        return 0;
    }

    unsafe {
        if (*ptr.as_ptr()).has_dead_flag() {
            0
        } else if (*ptr.as_ptr()).dropping_state() != 0 {
            0  // 或者返回一個表示「正在 drop」的特殊值
        } else {
            (*ptr.as_ptr()).ref_count().get()
        }
    }
}
```

為 `Weak::weak_count()` 添加基本檢查：

```rust
pub fn weak_count(&self) -> usize {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return 0;
    };
    let ptr_addr = ptr.as_ptr() as usize;
    let alignment = std::mem::align_of::<GcBox<T>>();
    if ptr_addr % alignment != 0 {
        return 0;
    }

    unsafe {
        if (*ptr.as_ptr()).has_dead_flag() || (*ptr.as_ptr()).dropping_state() != 0 {
            0
        } else {
            (*ptr.as_ptr()).weak_count()
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在物件正在被 drop 的過程中，ref_count 和 weak_count 可能正在變化。讀取這些值可能會得到瞬時的、中間狀態的值，這對於呼叫者來說是無意義的。在 cyclic reference GC 中，我們應該確保在物件銷毀過程中，這些計數接口返回一致的值。

**Rustacean (Soundness 觀點):**
這不是傳統的 soundness 問題（不會導致 UB），但可能導致邏輯錯誤。呼叫者可能根據這些計數值做出錯誤的假設。例如，如果 `strong_count()` 在物件正在 drop 時返回非零值，呼叫者可能會錯誤地認為物件仍然活著。

**Geohot (Exploit 攻擊觀點):**
在並發場景下，如果攻擊者能夠精確控制 timing，可能利用這個邊界條件讀取到不一致的計數值，進一步探索記憶體佈局。不過由於沒有直接的記憶體讀取漏洞，這個攻擊面的影響有限。
