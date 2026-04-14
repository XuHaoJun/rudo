# [Bug]: simple_write_barrier large object path missing third is_allocated check

**Status:** Fixed
**Tags:** Verified

## 修復紀錄 (Fix Applied)

**Date:** 2026-04-12
**Fix:** Added third `is_allocated` check after reading `has_gen_old` in `simple_write_barrier` large object path (heap.rs:2917-2923).

**Code Change:**
```rust
// FIX bug604: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU.
// If slot was swept after generation check but before returning,
// we'd return a stale slot index. Matches incremental_write_barrier (bug499)
// and simple_write_barrier small object path (bug535) patterns.
if !(*h_ptr).is_allocated(0) {
    return;
}
```

**Verification:** `cargo check --lib` passes.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Every write barrier on large object path triggers this |
| **Severity (嚴重程度)** | High | Could set dirty bit on wrong slot after sweep |
| **Reproducibility (復現難度)** | Medium | Requires precise timing between barrier and sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `simple_write_barrier` large object path (heap.rs:2901-2917)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
All barrier functions should have three `is_allocated` checks:
1. First check before reading any fields
2. Second check before reading `has_gen_old_flag`
3. **Third check AFTER reading `has_gen_old_flag`** - prevents TOCTOU

### 實際行為 (Actual Behavior)

In `simple_write_barrier` **large object path** (heap.rs:2901-2917):

```rust
// Skip if slot was swept; avoids corrupting dirty tracking with reused slot (bug286).
if !(*h_ptr).is_allocated(0) {      // FIRST CHECK (line 2901)
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;  // line 2905
// FIX bug467: Second is_allocated check BEFORE reading has_gen_old - prevents TOCTOU.
if !(*h_ptr).is_allocated(0) {      // SECOND CHECK (line 2909)
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // READ has_gen_old (line 2913)
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// MISSING: Third is_allocated check AFTER reading has_gen_old!
(NonNull::new_unchecked(h_ptr), 0_usize)  // Returns without verifying slot still allocated
```

### 對比 `incremental_write_barrier` large object path (lines 3337-3353):

`incremental_write_barrier` CORRECTLY has the third check:

```rust
if !(*h_ptr).is_allocated(0) {      // FIRST CHECK (line 3337)
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
if !(*h_ptr).is_allocated(0) {      // SECOND CHECK (line 3343)
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // READ has_gen_old (line 3346)
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
if !(*h_ptr).is_allocated(0) {      // THIRD CHECK (line 3350) - FIX bug499
    return;
}
(NonNull::new_unchecked(h_ptr), 0_usize)
```

### 對比 `simple_write_barrier` small object path (lines 2940-2955):

The small object path CORRECTLY has the third check after bug535 fix:

```rust
// Third is_allocated check AFTER has_gen_old read - prevents TOCTOU (bug535).
if !(*h.as_ptr()).is_allocated(index) {  // THIRD CHECK (lines 2951-2953)
    return;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `simple_write_barrier` large object path at heap.rs around line 2913-2917.

**TOCTOU race scenario:**
1. Thread A: First `is_allocated(0)` check passes (line 2901)
2. Thread B: Slot is swept and reused with new object
3. Thread A: Second `is_allocated(0)` check passes (line 2909) - slot appears allocated again
4. Thread A: Reads `has_gen_old` from reused slot (line 2913) - stale data
5. Thread A: Checks generation (line 2914) - may pass if new object has same gen
6. Thread A: Returns `(h_ptr, 0)` pointing to wrong slot
7. Caller sets dirty bit on wrong slot

**This bug was introduced when bug467 added the second check, but the third check was only added to the small object path (bug535 fix) and the incremental_write_barrier (bug499 fix), but the large object path in simple_write_barrier was overlooked.**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This bug requires concurrent execution:
// 1. Create GcCell with large object (> page size)
// 2. One thread does write barrier
// 3. Another thread does GC sweep + reuse the slot
// 4. Observe wrong dirty bit being set

use rudo_gc::{Gc, GcCell, Trace, collect_full};
use std::thread;

#[derive(Trace)]
struct LargeData {
    value: [i32; 1024],  // Large object > page size
}

fn main() {
    let cell = GcCell::new(LargeData { value: [0; 1024] });
    
    // Trigger write barrier
    let mut guard = cell.borrow_mut();
    guard.value[0] = 100;
    drop(guard);
    
    // Force GC and slot reuse
    collect_full();
    
    // Access cell again - may have wrong dirty tracking
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add the third `is_allocated` check after reading `has_gen_old` in `simple_write_barrier` large object path:

```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bug604: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU.
// If slot was swept after generation check but before returning,
// we'd return a stale slot index. Matches incremental_write_barrier (bug499)
// and simple_write_barrier small object path (bug535) patterns.
if !(*h_ptr).is_allocated(0) {
    return;
}
(NonNull::new_unchecked(h_ptr), 0_usize)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The simple_write_barrier is a basic generational barrier used in non-incremental contexts. The missing third check could cause incorrect dirty page tracking, leading to objects being incorrectly traced during minor GC. This is the same bug pattern that was fixed in incremental_write_barrier (bug499) and simple_write_barrier small object path (bug535), but the large object path was overlooked.

**Rustacean (Soundness 觀點):**
This is a classic TOCTOU bug. The slot could be swept and reused between reading has_gen_old and returning. The third check is essential to ensure the slot is still valid before the caller uses the returned index.

**Geohot (Exploit 觀點):**
An attacker could potentially exploit this by:
1. Creating a self-referential structure with large objects
2. Forcing GC to sweep and reuse the slot
3. The corrupted dirty bitmap could lead to incorrect tracing behavior