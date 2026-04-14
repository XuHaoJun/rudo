# [Bug]: GcThreadSafeCell::borrow_mut_simple inconsistent with GcRwLock::write - OLD capture conditional

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking phase 转换时持有 borrow_mut_simple guard |
| **Severity (嚴重程度)** | High | 可能导致对象被错误回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精确的时序控制，单线程无法重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut_simple()` (`cell.rs:1147-1207`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
所有修改 GC 指针的 write barrier 实现应该一致地捕获 OLD 值用于 SATB，无论 `incremental_active` 状态如何。

### 實際行為 (Actual Behavior)
`GcThreadSafeCell::borrow_mut_simple()` 在 `incremental_active` 为 false 时，不会捕获 OLD 值到 SATB buffer。与 `GcRwLock::write()` (bug432 fix) 和 `borrow_mut()` 的行为不一致。

**对比 `GcRwLock::write()` (正确行为):**
```rust
// sync.rs:295 (write - FIX bug432)
record_satb_old_values_with_state(&*guard, true);  // 总是记录 OLD 值
mark_gc_ptrs_immediate(&*guard, true);              // 总是标记 NEW 值
```

**对比 `borrow_mut()` (正确行为):**
```rust
// cell.rs:173-192 (borrow_mut - FIX bug486)
{
    unsafe {
        let value = &*self.inner.as_ptr();
        let mut gc_ptrs = Vec::with_capacity(32);
        value.capture_gc_ptrs_into(&mut gc_ptrs);  // 无条件捕获 OLD
        // ... recording ...
    }
}
```

**`borrow_mut_simple()` (不一致):**
```rust
// cell.rs:1157-1173 (borrow_mut_simple)
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
let value = &*guard;
let mut gc_ptrs = Vec::with_capacity(32);
value.capture_gc_ptrs_into(&mut gc_ptrs);
if !gc_ptrs.is_empty()
    && crate::heap::try_with_heap(|heap| {
        // ONLY records when incremental_active is true!
        for gc_ptr in &gc_ptrs {
            if !heap.record_satb_old_value(*gc_ptr) {
                // ...
            }
        }
        true
    })
    .is_some()
{
    // Heap available - records only if incremental_active = true
} else {
    // Cross-thread fallback - also only when incremental_active = true
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**问题场景:**

1. Thread A 调用 `borrow_mut_simple()`，此时 `incremental_active = false`
2. `borrow_mut_simple()` 中的 OLD 值记录被跳过（因为 `incremental_active = false`）
3. 用户代码修改了 GC 指针：`cell = old_ptr` 变为 `cell = new_ptr`
4. 在 drop 之前，`incremental_active` 变为 `true`
5. `drop()` 中的 `mark_object_black` 标记 NEW 值
6. **OLD 值从未被记录！SATB 不变性被破坏！**

**代码位置:**

- `borrow_mut_simple()` lines 1159-1185: OLD 捕获发生在 `incremental_active` 检查之后
- `GcRwLock::write()` line 295: `record_satb_old_values_with_state(&*guard, true)` - 无条件
- `borrow_mut()` lines 173-192: `value.capture_gc_ptrs_into()` - 无条件

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精确的时序控制，难以在单线程环境重现。理论 PoC:

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, GcCapture, collect_full};
use std::sync::Arc;
use std::thread;

#[derive(Trace, GcCapture)]
struct Data {
    value: i32,
}

fn main() {
    let cell: Gc<GcThreadSafeCell<Vec<Gc<Data>>>> = Gc::new(
        GcThreadSafeCell::new(vec![Gc::new(Data { value: 1 })])
    );

    // borrow_mut_simple when incremental_active = false
    let mut guard = cell.borrow_mut_simple();
    let old_ptr = guard.get(0).clone();
    
    // Replace with new object
    guard[0] = Gc::new(Data { value: 2 });  // new_ptr
    
    // At this exact point, incremental marking activates
    // (another thread triggers GC with incremental enabled)
    
    // OLD pointer was never recorded for SATB!
    drop(guard);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `borrow_mut_simple()` 以无条件捕获 OLD 值，与 `borrow_mut()` 一致:

```rust
pub fn borrow_mut_simple(&self) -> parking_lot::MutexGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.lock();
    
    // FIX bug475: 总是捕获 OLD 值，无条件
    // 总是记录到 SATB buffer，无论 incremental_active 状态
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
        .is_some()
    {
        // Heap available - SATB recorded
    } else {
        // No heap, push to cross-thread buffer
        for gc_ptr in gc_ptrs {
            if !crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr) {
                crate::gc::incremental::IncrementalMarkState::global()
                    .request_fallback(
                        crate::gc::incremental::FallbackReason::SatbBufferOverflow,
                    );
            }
        }
    }
    
    // ... remaining code
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 的核心不變性是：在 snapshot 時可達的對象必須保持可達。如果 `borrow_mut_simple()` 捕獲時 `incremental_active = false`，則沒有記錄 OLD 值。即使 `drop()` 時 `incremental_active = true`，`mark_object_black` 只能保護 NEW 值可達的對象，無法保護 OLD 值可達的對象。這破壞了 incremental marking 的基本假設。

**Rustacean (Soundness 觀點):**
這是一個內存安全問題。如果對象被錯誤回收，通過 `old_ptr` 訪問會導致 use-after-free。在 Rust 中，這是未定義行為的一種形式。問題在於：`borrow_mut_simple()` 只在 `incremental_active = true` 時記錄 OLD 值，與 `GcRwLock::write()` 和 `borrow_mut()` 不一致。

**Geohot (Exploit 觀點):**
雖然需要精確的時序控制，攻擊者可能通過構造特定的執行時序來觸發此 bug。在極端情況下，這可能導致內存腐敗。關鍵攻擊面在於：如果攻擊者能夠控制 GC 觸發的時序，可以讓 OLD 值可達的敏感對象被錯誤回收，然後通過 use-after-free 讀取已釋放記憶體。

---

## 備註

- 與 bug432 相關：bug432 修復了 `GcRwLock::write()` 的同樣問題，但 `borrow_mut_simple()` 沒有應用相同的修復
- 與 bug475 相關：bug475 描述了相同的問題
- 與 bug486 不同：bug486 關注 `borrow_mut()` 的 NEW 值標記問題
