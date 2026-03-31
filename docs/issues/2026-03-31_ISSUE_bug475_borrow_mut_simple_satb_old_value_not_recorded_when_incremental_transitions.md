# [Bug]: GcThreadSafeCell::borrow_mut_simple 當 incremental_active 從 false 轉換為 true 時，SATB OLD 值未被記錄

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 borrow_mut_simple() 和 drop() 之间 incremental marking phase 发生转换 |
| **Severity (嚴重程度)** | High | 可能导致年轻对象被错误回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精确的时序控制，单线程无法重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut_simple()` (`cell.rs:1147-1207`) 和 `GcThreadSafeRefMut::drop()` (`cell.rs:1483-1509`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
SATB (Snapshot-At-The-Beginning) barrier 应该在修改前捕获 GC 指针的 OLD 值（在被覆写之前），确保通过 OLD 值可达的对象在 marking 阶段被保留。

### 實際行為 (Actual Behavior)
`GcThreadSafeCell::borrow_mut_simple()` 在 `incremental_active` 为 false 时，不会记录 OLD 值到 SATB buffer。然后在 `GcThreadSafeRefMut::drop()` 中，如果 `incremental_active` 已经变为 true，只会通过 `mark_object_black` 标记 NEW 值，而 OLD 值没有被记录，可能导致 OLD 对象被错误回收。

### 對比 `borrow_mut()` (正確的行為)

`borrow_mut()` 在返回 guard 之前立即标记所有 GC 指针为黑色：

```rust
// cell.rs:1116-1128 (borrow_mut)
if incremental_active {
    // 在返回 guard 前立即标记 - 保护 NEW 值
    unsafe {
        let guard_ref = &*result;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            for gc_ptr in new_gc_ptrs {
                let _ = crate::gc::incremental::mark_object_black(
                    gc_ptr.as_ptr() as *const u8
                );
            }
        }
    }
}
```

而 `borrow_mut_simple()` 在 `borrow_mut_simple()` 时间接标记（在 drop 时）：

```rust
// cell.rs:1195-1207 (borrow_mut_simple - 在 drop 时标记)
if incremental_active {
    unsafe {
        let guard_ref = &*guard;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        guard_ref.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            for gc_ptr in new_gc_ptrs {
                let _ = crate::gc::incremental::mark_object_black(
                    gc_ptr.as_ptr() as *const u8
                );
            }
        }
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題場景

1. Thread A 調用 `borrow_mut_simple()`，此時 `incremental_active = false`
2. `borrow_mut_simple()` 中的 OLD 值記錄被跳過（因為 `incremental_active = false`）
3. 用戶代碼修改了 GC 指針：`cell = old_ptr` 變為 `cell = new_ptr`
4. 在 drop 之前，`incremental_active` 變為 `true`
5. Thread A 調用 `drop()`
6. `drop()` 中的 `mark_object_black` 只標記 NEW 值可達的對象
7. **從 OLD 值可達但從 NEW 值不可達的對象可能會被錯誤回收！**

### 時序問題

```
T1: Thread A calls borrow_mut_simple(), incremental_active = false
T2: Thread A modifies cell = old_ptr -> new_ptr
T3: Collector starts incremental marking, incremental_active = true
T4: Thread A drops guard
T5: capture_gc_ptrs_into captures new_ptr (current value)
T6: mark_object_black(new_ptr) marks objects reachable from new_ptr
T7: Objects only reachable from old_ptr may be prematurely collected!
```

### 對比 GcRwLock (已修復)

Bug432 修復了 `GcRwLock::write()` - 總是記錄 OLD 值：

```rust
// sync.rs:295 (write - FIX bug432)
record_satb_old_values_with_state(&*guard, true);  // 總是記錄
```

但 `borrow_mut_simple()` 仍然只在 `incremental_active = true` 時記錄：

```rust
// cell.rs:1159-1190 (borrow_mut_simple)
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
if incremental_active {  // 問題：只在增量標記啟用時記錄
    // ... 記錄 OLD 值
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要精確的時序控制，難以在單線程環境重現。理論 PoC：

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, GcCapture, collect_full, set_incremental_config, IncrementalConfig};
use std::sync::Arc;
use std::thread;

#[derive(Trace, GcCapture)]
struct Data {
    value: i32,
}

fn main() {
    set_incremental_config(IncrementalConfig {
        enabled: true,
        dirty_pages_threshold: 10,
        slice_duration_ns: 1_000_000,
    });

    let cell: Gc<GcThreadSafeCell<Vec<Gc<Data>>>> = Gc::new(GcThreadSafeCell::new(vec![
        Gc::new(Data { value: 1 }),  // old_ptr
    ]));

    // borrow_mut_simple when incremental_active = false
    let mut guard = cell.borrow_mut_simple();
    let old_ptr = guard.get(0).clone();
    
    // Replace with new object
    guard[0] = Gc::new(Data { value: 2 });  // new_ptr
    
    // At this exact point, incremental marking activates
    // (another thread triggers GC with incremental enabled)
    
    // When guard drops, only new_ptr is marked black
    // Objects only reachable from old_ptr may be collected!
    drop(guard);
    
    // If old_ptr's object was only reachable from the cell slot
    // and not from any other root, it may be prematurely collected
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1: 在 borrow_mut_simple() 強制記錄 SATB（推薦）

無論 `incremental_active` 狀態如何，在 `borrow_mut_simple()` 時都記錄 SATB OLD 值：

```rust
pub fn borrow_mut_simple(&self) -> parking_lot::MutexGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.lock();
    
    // 總是記錄 OLD 值，無論 incremental_active 狀態
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    if true {  // 總是記錄，或者：
        let value = &*guard;
        let mut gc_ptrs = Vec::with_capacity(32);
        value.capture_gc_ptrs_into(&mut gc_ptrs);
        if !gc_ptrs.is_empty()
            && crate::heap::try_with_heap(|heap| {
                for gc_ptr in &gc_ptrs {
                    if !heap.record_satb_old_value(*gc_ptr) {
                        crate::gc::incremental::IncrementalMarkState::global()
                            .request_fallback(
                                crate::gc::incremental::FallbackReason::SatbBufferOverflow,
                            );
                        break;
                    }
                }
                true
            })
            // ... cross-thread fallback
        }
    }
    
    // ... 其餘代碼
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 的核心不變性是：在 snapshot 時可達的對象必須保持可達。如果 `borrow_mut_simple()` 捕獲時 `incremental_active = false`，則沒有記錄 OLD 值。即使 `drop()` 時 `incremental_active = true`，`mark_object_black` 只能保護 NEW 值可達的對象，無法保護 OLD 值可達的對象。這破壞了 incremental marking 的基本假設。

**Rustacean (Soundness 觀點):**
這是一個內存安全問題。如果對象被錯誤回收，通過 `old_ptr` 訪問會導致 use-after-free。在 Rust 中，這是未定義行為的一種形式。問題在於：`borrow_mut_simple()` 只在 `incremental_active = true` 時記錄 OLD 值，但 `drop()` 只標記 NEW 值。

**Geohot (Exploit 觀點):**
雖然需要精確的時序控制，攻擊者可能通過構造特定的執行時序來觸發此 bug。在極端情況下，這可能導致內存腐敗。關鍵攻擊面在於：如果攻擊者能夠控制 GC 觸發的時序，可以讓 OLD 值可達的敏感對象被錯誤回收，然後通過 use-after-free 讀取已釋放記憶體。

---

## 備註

- 與 bug432 相關：bug432 修復了 `GcRwLock::write()` 的同樣問題，但 `borrow_mut_simple()` 沒有應用相同的修復
- 與 bug411 不同：bug411 修復了 `GcThreadSafeRefMut::drop()` 使用過時的 `incremental_active` 值；本 bug 關注 `borrow_mut_simple()` 根本沒有記錄 OLD 值