# [Bug]: GcThreadSafeCell::borrow_mut_simple 缺少 mark_object_black 導致新指標在增量標記時未被標記

**Status:** Invalid
**Tags:** Duplicate

**Duplicate Note:** This bug was already fixed in commit `be9c4a0` ("fix(cell): add mark_object_black to borrow_mut_simple for incremental GC"). The current code at cell.rs:1192-1204 correctly includes `mark_object_black` call when `incremental_active` is true.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要使用 borrow_mut_simple 搭配增量標記啟用，且類型包含 GC 指標 |
| **Severity (嚴重程度)** | High | 可能導致新指標被錯誤回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Medium | 需要精確控制增量標記時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut_simple` (cell.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcThreadSafeCell::borrow_mut_simple` 應該在增量標記啟用時，與 `borrow_mut` 一樣標記新 GC 指標為黑色。

### 實際行為 (Actual Behavior)

`borrow_mut_simple` 在增量標記啟用時：
1. 正確捕获舊指標用於 SATB
2. 正確觸發寫屏障
3. **但沒有調用 `mark_object_black` 標記新指標**

對比 `borrow_mut` (cell.rs:1059-1131)：
- 增量啟用時會捕獲舊指標
- 會調用 `trigger_write_barrier_with_incremental`
- **會調用 `mark_object_black` 標記新指標**

而 `borrow_mut_simple` (cell.rs:1147-1207)：
- 增量啟用時會捕獲舊指標
- 會調用 `trigger_write_barrier_with_incremental`
- **沒有調用 `mark_object_black` 標記新指標**

關鍵代碼對比：

```rust
// GcThreadSafeCell::borrow_mut (lines 1109-1125)
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

```rust
// GcThreadSafeCell::borrow_mut_simple (lines 1188-1206) - 沒有 mark_object_black！
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `borrow_mut_simple` 中，`incremental_active` 被設為 `false`（line 1156），而標記新指標的邏輯只在 `incremental_active` 為 true 時執行（`borrow_mut` 中的 if 塊）。

因此 `borrow_mut_simple` 永远不会调用 `mark_object_black`，即使增量标记已启用。

影響：
- 如果使用 `borrow_mut_simple` 寫入新的 GC 指標
- 增量標記正在運行
- 這些新指標不會被立即標記為黑色
- 可能導致在下次增量標記周期之前，這些指標引用的對象被錯誤回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, collect_full,GcCell};
use std::sync::Arc;
use std::thread;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace, Clone)]
struct Data {
    value: GcCell<i32>,
}

fn main() {
    // 假設增量標記已啟用
    let cell = Arc::new(GcThreadSafeCell::new(Data {
        value: GcCell::new(42)
    }));
    
    let cell2 = cell.clone();
    
    // 在新線程中使用 borrow_mut_simple
    let handle = thread::spawn(move || {
        // borrow_mut_simple - 新指標不會被標記
        let mut data = cell2.borrow_mut_simple();
        data.value = GcCell::new(100); // 新指標未被 mark_object_black
    });
    
    handle.join().unwrap();
    
    // 如果 GC 在這裡運行，增量標記可能會miss新指標
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `borrow_mut_simple` 中添加 `mark_object_black` 調用：

```rust
pub fn borrow_mut_simple(&self) -> parking_lot::MutexGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.lock();

    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    if incremental_active {
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
            // Heap available, SATB recorded in thread-local buffer
        } else {
            for gc_ptr in gc_ptrs {
                if !crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr) {
                    crate::gc::incremental::IncrementalMarkState::global().request_fallback(
                        crate::gc::incremental::FallbackReason::SatbBufferOverflow,
                    );
                }
            }
        }
    }

    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);

    // FIX: Add mark_object_black for incremental marking
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

    guard
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在增量標記期間，新指標應該立即被標記為黑色（黑分配優化）。`borrow_mut_simple` 捕获了舊指標用於 SATB，但沒有標記新指標。這可能導致新分配的對象在增量標記周期之間被錯誤回收。

**Rustacean (Soundness 觀點):**
如果 `borrow_mut_simple` 的文檔說「不需要 GcCapture」（但實際上有 `T: GcCapture` 約束），這表明 API 設計存在不一致。如果用戶錯誤地使用此方法與包含 GC 指標的類型，可能會導致記憶體安全問題。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制何時調用 GC，並且 `borrow_mut_simple` 在增量標記期間未被標記，攻擊者可能能夠導致對象被錯誤回收並重新分配給攻擊者控制的數據，從而實現 use-after-free。

---

## 相關 Issue

- bug174: borrow_mut_simple 捕獲舊 GC 指針的 SATB 問題
- bug192: 即時標記 GC 指針的必要性
- bug301: mark_object_black 應該只在增量標記期間調用
- bug302: 增量/世代屏障邏輯不一致
