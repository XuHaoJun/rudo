# [Bug]: GcThreadSafeCell::borrow_mut 缺少即時標記 - 與 GcCell 行為不一致

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需多執行緒：mutator 執行 borrow_mut 與 collector 啟動標記的時序交錯 |
| **Severity (嚴重程度)** | `High` | 導致新指標指向的年輕物件被錯誤回收，造成 use-after-free |
| **Reproducibility (Reproducibility)** | `Low` | 需精確時序，單執行緒無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut`, `cell.rs:1041-1087`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcThreadSafeCell::borrow_mut()` 應該與 `GcCell::borrow_mut()` 行為一致：在 mutation 完成後立即標記新的 GC 指針為黑色，確保在 generational barrier 或 incremental marking 啟用時，這些指標指向的物件不會被錯誤回收。

### 實際行為 (Actual Behavior)
在 `GcThreadSafeCell::borrow_mut()` 中，新的 GC 指針**不會**被立即標記。標記被延遲到 `GcThreadSafeRefMut::drop()` 時才執行。

這與 `GcCell::borrow_mut()` (lines 193-207) 的行為不同，後者會在 mutation 後立即調用 `mark_object_black()`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/cell.rs:1041-1087`:

```rust
// GcThreadSafeCell::borrow_mut() (lines 1041-1087)
pub fn borrow_mut(&self) -> GcThreadSafeRefMut<'_, T>
where
    T: Trace + GcCapture,
{
    let guard = self.inner.lock();

    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();

    if incremental_active {
        // 只記錄 SATB - 當 incremental 啟用時
        // ...
    }

    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);

    // 問題：沒有立即標記新的 GC 指針！
    // 標記被延遲到 GcThreadSafeRefMut::drop()

    GcThreadSafeRefMut {
        inner: guard,
        _marker: std::marker::PhantomData,
    }
}
```

對比 `GcCell::borrow_mut()` (lines 193-207):

```rust
if barrier_active {  // barrier_active = generational_active || incremental_active
    unsafe {
        let new_value = &*result;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            crate::heap::with_heap(|_heap| {
                for gc_ptr in new_gc_ptrs {
                    let _ = crate::gc::incremental::mark_object_black(
                        gc_ptr.as_ptr() as *const u8
                    );
                }
            });
        }
    }
}
```

**Race Condition 時序**:
1. Thread A (mutator): 執行 `borrow_mut()`，mutation 完成，新 GC 指針存在於 value 中
2. Thread A: `borrow_mut()` 返回，`GcThreadSafeRefMut` 被創建
3. Thread B (collector): 啟動 incremental/generational marking
4. Thread A: `GcThreadSafeRefMut` 被 drop（可能稍後才發生）
5. 在步驟 3 和 4 之間：新指標未被標記，可能導致其所指向的年輕物件被錯誤回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒環境觸發 race window
// 此 bug 難以在單執行緒環境重現
// 建議使用 ThreadSanitizer 或設計特定時序的 stress test
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcThreadSafeCell::borrow_mut()` 中，在返回 `GcThreadSafeRefMut` 之前，新增與 `GcCell::borrow_mut()` 相同的即時標記邏輯：

```rust
// 在 trigger_write_barrier_with_incremental() 之後，return GcThreadSafeRefMut 之前
let barrier_active = generational_active || incremental_active;

if barrier_active {
    unsafe {
        let guard_ref = &*guard;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        guard_ref.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            crate::heap::with_heap(|_heap| {
                for gc_ptr in new_gc_ptrs {
                    let _ = crate::gc::incremental::mark_object_black(
                        gc_ptr.as_ptr() as *const u8
                    );
                }
            });
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是典型的「延遲標記」與「即時標記」的權衡問題。即時標記確保物件在 mutation 後立即被保護，但會增加每次 mutation 的開銷。延遲標記（在 Drop 時）可以 batching，但會產生 race window。在generational GC 中，這個 window 特別危險，因為年輕物件可能在此期間被錯誤回收。

**Rustacean (Soundness 觀點):**
這不會導致明確的 UB（記憶體仍然有效），但會導致記憶體安全問題：物件被錯誤回收後可能被重用，導致 use-after-free。這種 bug 很難調試，因為它是並發的且依賴時序。

**Geohot (Exploit 觀點):**
攻擊者可以透過精確控制 GC 時機來利用這個 race window。雖然難度較高，但這是一個確實存在的攻擊面。需要仔細考慮在何處添加同步點。
