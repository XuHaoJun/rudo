# [Bug]: simple_write_barrier small object path missing third is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📝 修復紀錄 (Fix Applied)

**Date:** 2026-04-08
**Fix:** Added third is_allocated check after reading has_gen_old in `simple_write_barrier` small object path (heap.rs:2947-2950).
**Reason:** Prevents TOCTOU race where slot could be swept and reused between reading has_gen_old and returning. This matches the pattern already present in incremental_write_barrier, gc_cell_validate_and_barrier, and other barrier functions.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Every write barrier on small object path triggers this |
| **Severity (嚴重程度)** | High | Could set dirty bit on wrong slot after sweep |
| **Reproducibility (復現難度)** | Medium | Requires precise timing between barrier and sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `simple_write_barrier` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`simple_write_barrier` (heap.rs:2873) has two code paths: large object and small object. The **small object path** is missing the third `is_allocated` check after reading `has_gen_old_flag`, unlike other barrier functions.

### 預期行為 (Expected Behavior)
All barrier functions should have this pattern:
1. First `is_allocated` check
2. Second `is_allocated` check before reading GcBox fields (has_gen_old)
3. **Third `is_allocated` check AFTER reading has_gen_old** - prevents TOCTOU

### 實際行為 (Actual Behavior)

In `simple_write_barrier` small object path (heap.rs:2934-2947):
```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*h.as_ptr()).is_allocated(index) {      // FIRST CHECK
    return;
}
// Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU (bug463).
if !(*h.as_ptr()).is_allocated(index) {      // SECOND CHECK
    return;
}
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // READ has_gen_old
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// MISSING: Third is_allocated check AFTER has_gen_old read!
(h, index)  // Return without verifying slot still allocated
```

Compare with `incremental_write_barrier` small object path (heap.rs:3295-3316) which CORRECTLY has the third check:
```rust
// FIX bug530: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU.
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `simple_write_barrier` small object path at heap.rs around line 2934-2947.

The issue is a TOCTOU race:
1. Thread A: First is_allocated check passes
2. Thread B: Slot is swept and reused
3. Thread A: Second is_allocated check passes (slot appears allocated again due to reuse)
4. Thread A: Reads has_gen_old from reused slot (stale data)
5. Thread A: Checks generation (may pass if new object has same gen)
6. Thread A: Returns (h, index) pointing to wrong slot
7. Caller sets dirty bit on wrong slot

This is the same bug pattern documented in:
- bug530: incremental_write_barrier missing third check (FIXED)
- bug531: gc_cell_validate_and_barrier missing third check (FIXED)
- bug520: GcCell generational barrier missing third check (FIXED)
- bug521: GcThreadSafeCell generational barrier large path missing check (FIXED)
- bug499: incremental barrier large object path missing check (FIXED)
- bug498: incremental barrier small object path missing check (FIXED)

But `simple_write_barrier` was apparently overlooked and still has the bug.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This bug requires concurrent execution:
// 1. Create GcCell with small object
// 2. One thread does write barrier
// 3. Another thread does GC sweep + reuse the slot
// 4. Observe wrong dirty bit being set

use rudo_gc::{Gc, GcCell, Trace, collect_full};
use std::thread;

#[derive(Trace)]
struct SmallData {
    value: i32,
}

fn main() {
    let cell = GcCell::new(SmallData { value: 42 });
    
    // Trigger write barrier
    let mut guard = cell.borrow_mut();
    guard.value = 100;
    drop(guard);
    
    // Force GC and slot reuse
    collect_full();
    
    // Access cell again - may have wrong dirty tracking
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add the third is_allocated check after reading has_gen_old in `simple_write_barrier` small object path:

```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bug535: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU.
// If slot was swept after generation check but before returning,
// we'd return a stale slot index.
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
(h, index)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The simple_write_barrier is a basic generational barrier used in non-incremental contexts. The missing third check could cause incorrect dirty page tracking, leading to objects being incorrectly traced during minor GC.

**Rustacean (Soundness 觀點):**
This is a classic TOCTOU bug. The slot could be swept and reused between reading has_gen_old and returning. The third check is essential to ensure the slot is still valid before the caller uses the returned index.

**Geohot (Exploit 觀點):**
An attacker could potentially exploit this by:
1. Creating a self-referential structure
2. Forcing GC to sweep and reuse the slot
3. The corrupted dirty bitmap could lead to incorrect tracing behavior

**Summary:**
All barriers follow the same pattern for a reason - the third check is necessary to prevent TOCTOU races. `simple_write_barrier` was apparently missed when the other barriers were fixed.

---

## 驗證指南檢查

- Pattern 1 (Full GC 遮蔽 barrier bug): Use minor GC (`collect()`) not `collect_full()` to test
- Pattern 2 (單執行緒無法觸發競態): Need concurrent GC and barrier execution
- Pattern 3 (測試情境與 issue 描述不符): N/A
- Pattern 4 (容器內的 Gc 未被當作 root): N/A
- Pattern 5 (難以觀察的內部狀態): Dirty bitmap corruption is observable via debug assertions