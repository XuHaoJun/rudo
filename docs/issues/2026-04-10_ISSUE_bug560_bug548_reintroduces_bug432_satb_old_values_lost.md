# [Bug]: bug548 的修復重新引入 bug432 - GcRwLock/GcMutex SATB OLD 值記錄丟失

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 lock acquisition 和 drop 之间 incremental marking phase 发生转换 |
| **Severity (嚴重程度)** | High | 可能导致年轻对象被错误回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精确的时序控制，单线程无法重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::write()`, `GcRwLock::try_write()`, `GcMutex::lock()`, `GcMutex::try_lock()` (`sync.rs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
SATB (Snapshot-At-The-Beginning) barrier 應該在修改前捕獲 GC 指針的 OLD 值（在被覆寫之前），確保通過 OLD 值可達的對象在 marking 階段被保留。

### 實際行為 (Actual Behavior)
當 `incremental_active` 在 `write()` 時為 `false`，但在 `drop()` 時變為 `true` 時，OLD 值未被記錄到 SATB buffer。只有當前值（ mutation 後的值）被捕獲並標記為 black，違反了 SATB 不變性。

### 對比正確行為 (bug432 修復)

bug432 正確修復了這個問題 - 在 `write()` 時總是記錄 OLD 值：

```rust
// sync.rs:291 (bug432 修復，正確)
record_satb_old_values_with_state(&*guard, true);  // 總是記錄
```

但 bug548 認為這會造成不必要的開銷，將其改回 `incremental_active`：

```rust
// sync.rs:291 (bug548 修復，錯誤 - 重新引入 bug432)
record_satb_old_values_with_state(&*guard, incremental_active);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題場景

1. Thread A 調用 `GcRwLock::write()`，此時 `incremental_active = false`
2. `record_satb_old_values_with_state(&*guard, incremental_active)` 是 no-op（因為 `incremental_active = false`）
3. Thread A 持有鎖期間修改了 GC 指針：假設 `vec[0] = old_ptr` 變為 `vec[0] = new_ptr`
4. 在 drop 之前，`incremental_active` 變為 `true`
5. Thread A 調用 `drop()`
6. `capture_gc_ptrs_into(&mut ptrs)` 捕獲 `new_ptr`（當前值），而不是 `old_ptr`
7. `mark_object_black` 標記 `new_ptr` 可達的對象為 black
8. **從 `old_ptr` 可達但從 `new_ptr` 不可達的對象可能被錯誤回收！**

### 時序問題

```
T1: Thread A acquires lock, incremental_active = false
T2: record_satb_old_values_with_state is no-op
T3: Thread A modifies vec[0] = old_ptr -> new_ptr
T4: Collector starts incremental marking, incremental_active = true
T5: Thread A drops lock
T6: capture_gc_ptrs_into captures new_ptr (current value)
T7: mark_object_black(new_ptr) marks objects reachable from new_ptr
T8: Objects only reachable from old_ptr are prematurely collected!
```

### bug548 修復的問題

bug548 聲稱 `record_satb_old_values_with_state(&*guard, true)` 會造成不必要的開銷：

```rust
// bug548 認為的問題：當 incremental marking 未啟動時，仍然記錄 SATB
record_satb_old_values_with_state(&*guard, true);  // 不必要的開銷
```

但這個「優化」破壞了正確性 - 犧牲了安全性來換取效能。

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

### 方案：回滾 bug548 的修復

回滾 bug548 的修改，恢復 bug432 的修復：

```rust
// sync.rs:291
record_satb_old_values_with_state(&*guard, true);  // 恢復 bug432 修復
```

**注意**：這會增加一些不必要的開銷（當 incremental marking 未啟動時也記錄），但這是保持 SATB 正確性所必需的。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 的核心不變性是：在 snapshot 時可達的對象必須保持可達。如果 `record_satb_old_values_with_state` 在 `incremental_active = false` 時是 no-op，則沒有記錄 OLD 值。即使 `drop()` 時 `incremental_active = true`，`mark_object_black` 只能保護 NEW 值可達的對象，無法保護 OLD 值可達的對象。這破壞了 incremental marking 的基本假設。

**Rustacean (Soundness 觀點):**
這是一個內存安全問題。如果對象被錯誤回收，通過 `old_ptr` 訪問會導致 use-after-free。在 Rust 中，這是未定義行為的一種形式。bug548 的「優化」犧牲了安全性來換取效能，這是不可接受的。

**Geohot (Exploit 觀點):**
雖然需要精確的時序控制，攻擊者可能通過構造特定的執行時序來觸發此 bug。在極端情況下，這可能導致內存腐敗。關鍵攻擊面在於：如果攻擊者能夠控制 GC 觸發的時序，可以讓 OLD 值可達的敏感對象被錯誤回收，然後通過 use-after-free 讀取已釋放記憶體。

---

## 備註

- 與 bug432 相同：bug548 的修復重新引入了 bug432 的問題
- bug548 聲稱這是「不必要的開銷」，但實際上是保持正確性所必需的代價
- 建議：不要在安全性和效能之間做不正確的權衡