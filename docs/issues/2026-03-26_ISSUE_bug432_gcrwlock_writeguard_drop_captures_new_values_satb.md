# [Bug]: GcRwLockWriteGuard::drop() captures NEW values instead of OLD for SATB barrier

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 lock acquisition 和 drop 之间 incremental marking phase 发生转换 |
| **Severity (嚴重程度)** | High | 可能导致年轻对象被错误回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精确的时序控制，单线程无法重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop()` (`sync.rs:464-490`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
SATB (Snapshot-At-The-Beginning) barrier 应该在修改前捕获 GC 指针的 OLD 值（在被覆写之前），确保通过 OLD 值可达的对象在 marking 阶段被保留。

### 實際行為 (Actual Behavior)
`GcRwLockWriteGuard::drop()` 在 `incremental_active` 从 false 变为 true 时，捕获的是 CURRENT 值（修改后的值）而不是 OLD 值（修改前的值）：

```rust
// sync.rs:464-490 (GcRwLockWriteGuard::drop)
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);  // 捕获的是 DROP 时的当前值！

        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();

        if incremental_active {
            for gc_ptr in &ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
        // ...
    }
}
```

問題：捕獲的是 `ptrs`（當前值），而不是 mutation 前的 OLD 值。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題場景

1. Thread A 調用 `GcRwLock::write()`，此時 `incremental_active = false`
2. Thread A 持有鎖期間修改了 GC 指針：假設 `vec[0] = old_ptr` 變為 `vec[0] = new_ptr`
3. 在 drop 之前，`incremental_active` 變為 `true`
4. Thread A 調用 `drop()`
5. `capture_gc_ptrs_into(&mut ptrs)` 捕獲 `new_ptr`（當前值），而不是 `old_ptr`
6. `mark_object_black` 標記 `new_ptr` 可達的對象為 black
7. **從 `old_ptr` 可達但從 `new_ptr` 不可達的對象可能會被錯誤回收！**

### 時序問題

```
T1: Thread A acquires lock, incremental_active = false
T2: Thread A modifies vec[0] = old_ptr -> new_ptr
T3: Collector starts incremental marking, incremental_active = true
T4: Thread A drops lock
T5: capture_gc_ptrs_into captures new_ptr (current value)
T6: mark_object_black(new_ptr) marks objects reachable from new_ptr
T7: Objects only reachable from old_ptr may be prematurely collected!
```

### 對比正確路徑 (`write()`)

```rust
// sync.rs:283-300 (GcRwLock::write)
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.write();
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    record_satb_old_values_with_state(&*guard, incremental_active);  // 記錄 OLD 值
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    mark_gc_ptrs_immediate(&*guard, incremental_active);
    GcRwLockWriteGuard { guard, _marker: PhantomData }
}
```

當 `incremental_active = false` 時，`record_satb_old_values_with_state` 是 no-op（不記錄任何東西）。

### 為什麼 `mark_object_black` 不能替代 SATB

`mark_object_black` 直接標記對象為 black（live），適用於 NEW 值。但 SATB 需要記錄 OLD 值到 buffer，以確保在 snapshot 時可達的對象保持可達。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要精確的時序控制，難以在單線程環境重現。理論 PoC：

```rust
use rudo_gc::{Gc, GcRwLock, Trace, GcCapture, collect_full, set_incremental_config, IncrementalConfig};
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

    let rwlock: Gc<GcRwLock<Vec<Gc<Data>>>> = Gc::new(GcRwLock::new(vec![
        Gc::new(Data { value: 1 }),  // old_ptr
    ]));

    // Thread A acquires lock when incremental_active = false
    let mut guard = rwlock.write();
    let old_ptr = guard.get(0).clone();
    
    // Replace with new object
    guard[0] = Gc::new(Data { value: 2 });  // new_ptr
    
    // At this exact point, incremental marking activates
    // (another thread triggers GC with incremental enabled)
    
    // When guard drops, ptrs captures new_ptr, not old_ptr
    // Objects only reachable from old_ptr may be collected!
    drop(guard);
    
    // If old_ptr's object was only reachable from the vector slot
    // and not from any other root, it may be prematurely collected
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1: 在 write() 強制記錄 SATB（推薦）

無論 `incremental_active` 狀態如何，在 `write()` 時都記錄 SATB OLD 值：

```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.write();
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    
    // 總是記錄 OLD 值，無論 incremental_active 狀態
    record_satb_old_values_with_state(&*guard, true);  // 強制啟用
    
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    mark_gc_ptrs_immediate(&*guard, incremental_active);
    GcRwLockWriteGuard { guard, _marker: PhantomData }
}
```

缺點：可能增加無效的 SATB 記錄。

### 方案 2: 在 drop() 特殊處理

在 `drop()` 中，如果檢測到 barrier 狀態變化，需要特殊處理 OLD 值。但這需要存儲 mutation 前的值，不太實際。

### 方案 3: 文檔說明限制

如果這個行為是有意設計的（效能考慮），需要在文檔中明確說明 `GcRwLockWriteGuard` 不保證跨 barrier 狀態變化的 SATB 正確性。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 的核心不變性是：在 snapshot 時可達的對象必須保持可達。如果 `write()` 捕获時 `incremental_active = false`，則沒有記錄 OLD 值。即使 `drop()` 時 `incremental_active = true`，`mark_object_black` 只能保護 NEW 值可達的對象，無法保護 OLD 值可達的對象。這破壞了 incremental marking 的基本假設。

**Rustacean (Soundness 觀點):**
這是一個內存安全問題。如果對象被錯誤回收，通過 `old_ptr` 訪問會導致 use-after-free。在 Rust 中，這是未定義行為的一種形式。問題在於：1) `record_satb_old_values_with_state` 在 `incremental_active = false` 時是 no-op；2) `drop()` 捕獲的是 mutation 後的值。

**Geohot (Exploit 觀點):**
雖然需要精確的時序控制，攻擊者可能通過構造特定的執行時序來觸發此 bug。在極端情況下，這可能導致內存腐敗。關鍵攻擊面在於：如果攻擊者能夠控制 GC 觸發的時序，可以讓 OLD 值可達的敏感對象被錯誤回收，然後通過 use-after-free 讀取已釋放記憶體。

---

## 備註

- 與 bug409 相關但不同：bug409 關注 `unified_write_barrier` 使用過時的 `incremental_active` 值；本 bug 關注 `mark_object_black` 使用錯誤的值（NEW 而不是 OLD）
- 這個 bug 影響 SATB 的正確性，但 `generational barrier` 提供了一定程度的保護（通過 dirty page 標記）