# [Bug]: Gc::new_cyclic_weak DropGuard 邏輯錯誤可能導致不正確的 DEAD_FLAG 設置

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 只在 Gc::new_cyclic_weak 構造失敗時觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致後續錯誤的 weak reference 行爲 |
| **Reproducibility (復現難度)** | Medium | 需要讓 Gc::new_cyclic_weak 的 data_fn panic |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr.rs`, `Gc::new_cyclic_weak`, `DropGuard<T>`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`Gc::new_cyclic_weak` 函數中的 `DropGuard<T>::drop` 實現有邏輯錯誤。當 `data_fn` panic 時，`DropGuard::drop` 檢查 `weak_count` 是否大于 0 來決定調用 `mark_dead()` 還是 `dealloc`。

但 `Gc::new_cyclic_weak` 初始化 `GcBox` 時，`weak_count` 被設置為 `UNDER_CONSTRUCTION_FLAG`（一個非零值），而 `actual_count` 為 0。

在 `DropGuard::drop` 中：
```rust
let actual_count = raw_weak_count & !GcBox::<T>::FLAGS_MASK;
if actual_count > 0 || (raw_weak_count & GcBox::<T>::UNDER_CONSTRUCTION_FLAG) != 0 {
    (*self.gc_box_ptr.as_ptr()).mark_dead();
} else {
    with_heap(|heap| {
        heap.dealloc(self.ptr);
    });
}
```

當 `actual_count == 0` 且 `UNDER_CONSTRUCTION_FLAG` 設置時（構造失敗的預期狀態），代碼錯誤地調用 `mark_dead()` 而不是 `dealloc`。

### 預期行為
- 當構造失敗且 `actual_count == 0`（無 weak refs）時，應該調用 `dealloc` 釋放內存
- `mark_dead()` 應該只在有 weak references 存在時調用，因爲這些 refs 需要通過 `DEAD_FLAG` 來處理

### 實際行為
- 當 `actual_count == 0` 且 `UNDER_CONSTRUCTION_FLAG` 設置時，調用 `mark_dead()`
- 這會在 `weak_count` 中設置 `DEAD_FLAG`，但 `actual_count` 為 0
- 後續如果這個 GcBox slot 被重用，新的對象可能繼承這個 `DEAD_FLAG` 導致問題

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs` 的 `Gc::new_cyclic_weak` 函數中：

```rust
// Line 1500-1502: 初始化 weak_count 為 UNDER_CONSTRUCTION_FLAG
std::ptr::write(
    std::ptr::addr_of_mut!((*gc_box).weak_count),
    AtomicUsize::new(GcBox::<T>::UNDER_CONSTRUCTION_FLAG),
);
```

`UNDER_CONSTRUCTION_FLAG` 的值是 `1 << (usize::BITS - 2)`，這是一個非零值。

在 `DropGuard::drop` 中（Line 1469-1471）：
```rust
let raw_weak_count = (*self.gc_box_ptr.as_ptr()).weak_count_raw();
let actual_count = raw_weak_count & !GcBox::<T>::FLAGS_MASK;
```

`FLAGS_MASK` 定義為 `DEAD_FLAG | UNDER_CONSTRUCTION_FLAG | GEN_OLD_FLAG`，所以：
- `actual_count = UNDER_CONSTRUCTION_FLAG & ~FLAGS_MASK = 0`

當 `actual_count == 0` 且 `raw_weak_count & UNDER_CONSTRUCTION_FLAG != 0` 時：
```rust
if actual_count > 0 || (raw_weak_count & GcBox::<T>::UNDER_CONSTRUCTION_FLAG) != 0 {
    // 這個條件為 true，但 actual_count == 0，不應該調用 mark_dead
    (*self.gc_box_ptr.as_ptr()).mark_dead();
}
```

問題是條件 `actual_count > 0 || (raw_weak_count & GcBox::<T>::UNDER_CONSTRUCTION_FLAG) != 0` 包含了 `UNDER_CONSTRUCTION_FLAG` 的檢查，但這個標誌本身不應該觸發 `mark_dead()`。

正確的邏輯應該是：
- 如果 `actual_count > 0`（有真的 weak refs）：調用 `mark_dead()`
- 如果 `actual_count == 0`（無 weak refs）：調用 `dealloc`

`UNDER_CONSTRUCTION_FLAG` 的存在只是表示對象正在構造中，與是否有 weak refs 無關。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell};

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Gc<Node>>>,
    data: i32,
}

fn main() {
    // 嘗試創建 cyclic weak，但 data_fn panic
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Gc::new_cyclic_weak(|_weak_self| {
            panic!("intentional panic");
        });
    }));

    assert!(result.is_err()); // data_fn panicked

    // 第二次分配可能會重用之前的 slot
    // 如果 DropGuard 錯誤地設置了 DEAD_FLAG，可能導致問題
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `DropGuard::drop` 的邏輯，不要因爲 `UNDER_CONSTRUCTION_FLAG` 就調用 `mark_dead()`：

```rust
impl<T: Trace + ?Sized> Drop for DropGuard<T> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        unsafe {
            let raw_weak_count = (*self.gc_box_ptr.as_ptr()).weak_count_raw();
            let actual_count = raw_weak_count & !GcBox::<T>::FLAGS_MASK;
            
            // 只有當真的有 weak references 時才調用 mark_dead()
            // UNDER_CONSTRUCTION_FLAG 的存在不足以調用 mark_dead
            if actual_count > 0 {
                (*self.gc_box_ptr.as_ptr()).mark_dead();
            } else {
                with_heap(|heap| {
                    heap.dealloc(self.ptr);
                });
            }
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這個 bug 影響 `new_cyclic_weak` 的錯誤路徑
- 當構造失敗時對象應該被釋放，而不是被標記為 dead
- `DEAD_FLAG` 主要用於防止 UAF 和處理 weak refs 的併發訪問

**Rustacean (Soundness 觀點):**
- 這可能導致 slot 重用時 `DEAD_FLAG` 狀態不一致
- 雖然是錯誤路徑，但可能導致後續並髮問題

**Geohot (Exploit 觀點):**
- 如果 `DEAD_FLAG` 被錯誤設置，可能被利用來繞過 weak ref 檢查
- 但因爲是構造失敗路徑，利用難度較高
