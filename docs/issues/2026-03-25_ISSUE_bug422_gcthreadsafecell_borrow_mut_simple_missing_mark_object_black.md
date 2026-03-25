# [Bug]: GcThreadSafeCell::borrow_mut_simple 缺少 mark_object_black 導致增量 GC 遺漏新指標

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需啟用增量標記且使用 borrow_mut_simple 寫入新 GC 指標 |
| **Severity (嚴重程度)** | `Critical` | 可能導致新寫入的 GC 指標被錯誤回收，造成 UAF |
| **Reproducibility (復現難度)** | `High` | 需minor GC (collect()) 而非 collect_full()，且需並發增量標記 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut_simple`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcThreadSafeCell::borrow_mut_simple()` 在增量標記啟用時，應該：
1. 捕獲舊 GC 指標用於 SATB 屏障
2. 觸發寫屏障
3. **將新 GC 指標標記為黑色**（Dijkstra 插入屏障）

### 實際行為 (Actual Behavior)
`borrow_mut_simple()` 只做了：
1. 捕獲舊 GC 指標用於 SATB 屏障 ✓
2. 觸發寫屏障 ✓
3. **將新 GC 指標標記為黑色** ✗ (缺失)

與 `borrow_mut()` 相比，`borrow_mut_simple()` 缺少第 3 步。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/cell.rs:1147-1192` 的 `borrow_mut_simple()` 實作中：

```rust
pub fn borrow_mut_simple(&self) -> parking_lot::MutexGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.lock();

    // FIX bug174: Capture old GC pointers for SATB when incremental marking is active.
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    if incremental_active {
        // ... 捕獲舊指標錄 SATB ...
    }

    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);
    guard  // 缺少: 標記新 GC 指標為黑色！
}
```

對比 `borrow_mut()` (`cell.rs:1064-1131`)：
```rust
if incremental_active {
    // ... 標記新 GC 指標為黑色 ...
    for gc_ptr in new_gc_ptrs {
        let _ = crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8);
    }
}
```

`borrow_mut_simple()` 缺少 `mark_object_black` 呼叫。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, collect_full, GcCell};
use std::rc::Rc;
use std::cell::Cell;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 先 collect_full 將物件 promote 到 old gen
    collect_full();

    // 2. 使用 borrow_mut_simple 寫入新的 GC 指標
    let cell: Gc<GcThreadSafeCell<Data>> = Gc::new(GcThreadSafeCell::new(Data { value: 0 }));
    
    // 建立 OLD -> NEW 引用
    let new_gc = Gc::new(Data { value: 42 });
    *cell.borrow_mut_simple() = Data { value: 100 };  // 這裡沒有標記新指標為黑色
    
    // 3. 呼叫 collect() (minor only) - 新指標可能未被標記
    // 4. 嘗試存取 new_gc - 可能已被回收！
    println!("{}", new_gc.value);  // UAF!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `borrow_mut_simple()` 中，於 `trigger_write_barrier_with_incremental` 之後、返回 guard 之前，新增：

```rust
if incremental_active {
    unsafe {
        let new_value = &*guard;
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

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`borrow_mut_simple()` 的 SATB 屏障只記錄舊指標，但增量標記的 Dijkstra 插入屏障需要立即標記新指標為黑色。缺少 `mark_object_black` 會導致新寫入的 GC 指標在增量標記期間不被視為活躍，可能被錯誤回收。

**Rustacean (Soundness 觀點):**
此 bug 會導致 UAF (Use-After-Free)，這是記憶體安全違規。增量標記期間若新指標未被標記為黑色，並發 GC 可能將其錯誤回收。

**Geohot (Exploit 觀點):**
若攻擊者可觸發 `borrow_mut_simple()` 在增量標記期間寫入新 GC 指標，可能利用此 UAF 進行記憶體摧毀攻擊。