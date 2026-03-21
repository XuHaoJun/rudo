# [Bug]: GcThreadSafeCell::borrow_mut_gen_only 缺少世代寫屏障 - 導致 OLD→YOUNG 引用遺漏

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 使用 `borrow_mut_gen_only` 的開發者會預期有世代屏障，但實際沒有 |
| **Severity (嚴重程度)** | High | 導致 OLD→YOUNG 引用在minor GC時被遺漏，造成 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要minor GC + OLD object mutation + young object access |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut_gen_only`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`GcThreadSafeCell::borrow_mut_gen_only()` 方法聲稱提供「僅世代屏障」(Generational barrier only)，但實際上**完全沒有**調用任何寫屏障。

### 預期行為 (Expected Behavior)

根據文檔 (`cell.rs:44-51`):
```
| Method                   | Barrier Type              | T Bound   | Use Case                          |
| borrow_mut_gen_only()  | Generational only         | -         | Performance-critical code         |
```

應該在 mutate 時觸發世代寫屏障，標記舊世代物件的頁面為 dirty，使得下次 minor GC 能掃描這些 OLD→YOUNG 引用。

### 實際行為 (Actual Behavior)

`borrow_mut_gen_only` (`cell.rs:1157-1159`) 只獲取 mutex 鎖，**完全沒有**調用 `trigger_write_barrier_with_incremental`：

```rust
pub fn borrow_mut_gen_only(&self) -> parking_lot::MutexGuard<'_, T> {
    self.inner.lock()  // 沒有屏障！
}
```

相比較，`borrow_mut_simple` (`cell.rs:1129-1138`) 正確地調用了：
```rust
pub fn borrow_mut_simple(&self) -> parking_lot::MutexGuard<'_, T>
where
    T: Trace,
{
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);
    self.inner.lock()
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/cell.rs:1157-1159`，`borrow_mut_gen_only` 函數遺漏了對 `trigger_write_barrier_with_incremental` 的調用。

該函數負責：
1. 檢查物件是否在舊世代頁面 (`generation > 0`)
2. 檢查物件是否有 `gen_old_flag`
3. 如果是舊物件，標記頁面為 dirty

沒有這個調用，當 OLD 物件透過 `GcThreadSafeCell` mutate 指向 YOUNG 物件時，minor GC 無法追蹤這個引用，導致 young 物件被錯誤回收。

**注意:** 此 bug 與 bug312 類似，但 bug312 是針對 `GcCell::borrow_mut_gen_only`，而本 bug 是針對 `GcThreadSafeCell::borrow_mut_gen_only`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, collect_full};
use std::sync::Arc;
use std::thread;

#[derive(Trace)]
struct OldObj {
    young_ref: GcThreadSafeCell<YoungObj>,
}

#[derive(Trace)]
struct YoungObj {
    value: i32,
}

fn main() {
    // 1. 先 collect_full 將物件 promote 到 old gen
    let young = Gc::new(YoungObj { value: 42 });
    collect_full();
    
    // 2. 建立 OLD→YOUNG 引用 (透過 GcThreadSafeCell)
    let old = Gc::new(OldObj {
        young_ref: GcThreadSafeCell::new(YoungObj { value: 100 }),
    });
    collect_full(); // 確保 old 在 old gen
    
    // 3. 透過 borrow_mut_gen_only 建立 OLD→YOUNG 引用
    // Bug: 這裡應該記錄dirty page，但沒有！
    old.young_ref.borrow_mut_gen_only().value = 999;
    
    // 4. Drop strong ref to young
    drop(young);
    
    // 5. Minor GC (collect) - Bug: young物件可能在此被錯誤回收
    // 因為 OLD→YOUNG 引用沒有被記錄到 dirty list
    rudo_gc::collect();
    
    // 6. 存取 young 物件 - 可能 UAF
    println!("{}", old.young_ref.borrow().value);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `crates/rudo-gc/src/cell.rs:1157-1159` 的 `borrow_mut_gen_only` 函數中添加世代屏障調用：

```rust
pub fn borrow_mut_gen_only(&self) -> parking_lot::MutexGuard<'_, T> {
    // 添加世代屏障 (incremental_active = false)
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    if generational_active {
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut_gen_only", false);
    }
    
    self.inner.lock()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 破壞了世代 GC 的基本假設：舊世代物件對新世代物件的引用必須被記錄。當使用 `borrow_mut_gen_only` 時，開發者預期獲得效能提升（跳過 incremental marking），但這不應該犧牲正確性。世代屏障是 essential 的，不應該可選。

**Rustacean (Soundness 觀點):**
這不是傳統意義的 UB，但可能導致記憶體安全問題。如果 OLD→YOUNG 引用沒有被記錄，minor GC 會錯誤地回收仍然可達的 young 物件，後續存取會造成 use-after-free。

**Geohot (Exploit 觀點):**
攻擊者可以濫用這個行為來觸發 UAF。在高性能場景下使用 `borrow_mut_gen_only` 的應用程式可能成為目標。結合其他記憶體錯誤，這可以構成更複雜的攻擊。

---

## ✅ 修復記錄 (Fix Record)

**Date:** 2026-03-16
**Fix:** Added generational barrier call to `GcThreadSafeCell::borrow_mut_gen_only()` in `crates/rudo-gc/src/cell.rs:1157-1162`.

```rust
pub fn borrow_mut_gen_only(&self) -> parking_lot::MutexGuard<'_, T> {
    let incremental_active = false;
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);
    self.inner.lock()
}
```

**Verification:** All 11 gc_thread_safe_cell tests pass.
