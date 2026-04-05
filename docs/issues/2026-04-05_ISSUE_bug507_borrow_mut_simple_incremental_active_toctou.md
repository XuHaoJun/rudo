# [Bug]: GcThreadSafeRefMut::borrow_mut_simple incremental_active cached separately from generational_active - TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking phase 转换时持有 borrow_mut_simple guard |
| **Severity (嚴重程度)** | High | 可能导致对象被错误回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精确的时序控制，单线程无法重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeRefMut::borrow_mut_simple()` (`cell.rs:1147-1206`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`borrow_mut_simple()` 應該與 `GcRwLock::write()` (bug479 fix) 和 `borrow_mut()` 一致，在同一時間點緩存 `incremental_active` 和 `generational_active` 狀態，避免 TOCTOU 競爭。

### 實際行為 (Actual Behavior)
`borrow_mut_simple()` 在不同時間點檢查這兩個狀態：
- Line 1157: `incremental_active` 被緩存
- Line 1187: `generational_active` 被檢查

如果 incremental marking 在 line 1157 和 line 1187 之间变为 active，stale 的 `incremental_active = false` 会被传递给 barrier，导致 `record_in_remembered_buffer` (unified_write_barrier 中的 line 3185-3188) 被跳过。

**對比 `GcRwLock::write()` (正確行為):**
```rust
// sync.rs:290-291 (write)
let incremental_active = is_incremental_marking_active();
let generational_active = is_generational_barrier_active();
// 兩個狀態同時緩存
```

**`borrow_mut_simple()` (不一致):**
```rust
// cell.rs:1157
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
// ... SATB capture ...
// cell.rs:1187
let generational_active = crate::gc::incremental::is_generational_barrier_active();
// incremental_active 在 line 1157 緩存，generational_active 在 line 1187 檢查！
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題場景:**

1. Thread A 調用 `borrow_mut_simple()`，`incremental_active = false` (line 1157)
2. `incremental_active` 被緩存後，incremental marking 变为 active
3. Line 1187 檢查 `generational_active` (可能為 true)
4. `trigger_write_barrier_with_incremental(incremental_active=false, generational_active=true)` 被調用
5. 在 `unified_write_barrier` (line 3185-3188)，因為 `incremental_active = false` (stale)，`record_in_remembered_buffer` 被跳過
6. **即使 incremental marking 已激活，barrier 也無法正確處理！**

**代碼位置:**

- `borrow_mut_simple()` lines 1157, 1187: 兩個狀態在不同時間點檢查
- `borrow_mut()` lines 167-168: 兩個狀態同時緩存 (正確)
- `GcRwLock::write()` lines 290-291: 兩個狀態同時緩存 (正確)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制，難以在單線程環境重現。理論 PoC:

```rust
use rudo_gc::{Gc, GcThreadSafeRefMut, Trace, GcCapture, collect_full};
use std::sync::Arc;
use std::thread;

#[derive(Trace, GcCapture)]
struct Data {
    value: i32,
}

fn main() {
    let cell: Gc<GcThreadSafeRefMut<Vec<Gc<Data>>>> = Gc::new(
        GcThreadSafeRefMut::new(vec![Gc::new(Data { value: 1 })])
    );

    // borrow_mut_simple when incremental_active = false
    let mut guard = cell.borrow_mut_simple();
    
    // At this exact point, incremental marking activates
    // (another thread triggers GC with incremental enabled)
    // But incremental_active was cached as false!
    
    // The barrier will be triggered with stale incremental_active = false
    drop(guard);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `borrow_mut_simple()` 以同時緩存兩個狀態：

```rust
pub fn borrow_mut_simple(&self) -> parking_lot::MutexGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.lock();

    // FIX: Cache both barrier states together to avoid TOCTOU
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();

    // FIX bug475: Always capture old GC pointers for SATB, regardless of incremental_active.
    let value = &*guard;
    let mut gc_ptrs = Vec::with_capacity(32);
    value.capture_gc_ptrs_into(&mut gc_ptrs);
    // ... SATB recording (unconditional) ...

    self.trigger_write_barrier_with_incremental(incremental_active, generational_active);
    // ... remaining code ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
TOCTOU window between caching incremental_active and checking generational_active creates a race where the incremental barrier may not fire correctly even when incremental marking is active. The SATB invariant requires that when incremental marking is active, all OLD→NEW pointer writes be recorded in the remembered set.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. If the barrier doesn't fire correctly when incremental marking is active, objects reachable only through OLD→NEW pointers may be incorrectly collected, leading to use-after-free. This is undefined behavior in Rust.

**Geohot (Exploit 觀點):**
While precise timing control is required, an attacker who can influence GC timing could exploit this race condition. The window between line 1157 and line 1187 is small but real.

---

## 備註

- 與 bug173 相關：bug173 描述了 GcThreadSafeCell::borrow_mut_simple 的 TOCTOU 問題，但據稱已修復
- 與 bug475 相關：bug475 修復了總是捕獲 OLD 值，但沒有修復 TOCTOU 問題
- 與 bug479 相關：bug479 修復了 GcRwLock::write() 的同樣問題，但 borrow_mut_simple 沒有應用相同的修復
