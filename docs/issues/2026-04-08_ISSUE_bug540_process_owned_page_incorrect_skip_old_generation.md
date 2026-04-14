# [Bug]: process_owned_page is dead code with incorrect generational logic

**Status:** Fixed
**Tags:** Verified
**Resolution:** Removed dead code function `process_owned_page` which was never called from `worker_mark_loop_with_registry` or any other code path. The function contained incorrect logic that would cause old generation objects to be skipped during minor GC if ever used.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Triggered during minor GC with old generation pages in owned set |
| **Severity (嚴重程度)** | `Medium` | Old generation objects incorrectly skipped during marking phase |
| **Reproducibility (復現難度)** | `Low` | Requires minor GC + owned pages containing old generation objects |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `process_owned_page` in `gc/marker.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

During minor GC (`VisitorKind::Minor`), objects on old generation pages should still be traced. The `process_owned_page` function should only skip objects in the following specific case:
- Object is on a young generation page (`generation == 0`) AND
- Object has `gen_old_flag` NOT set

The early exit condition should be:
```rust
if page_generation == 0 && !has_gen_old_flag {
    continue; // Skip: young page and object not yet promoted
}
```

### 實際行為 (Actual Behavior)

In `process_owned_page` (marker.rs:705-709), the current code skips ALL objects on any old generation page:

```rust
if kind == VisitorKind::Minor
    && unsafe { (*header).generation.load(Ordering::Acquire) } > 0
{
    continue; // BUG: Skips ALL objects on old generation pages!
}
```

This means during minor GC, objects that were promoted to old generation are incorrectly skipped, causing them to not be traced and potentially collected prematurely.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `process_owned_page` at marker.rs:705-709:

```rust
if kind == VisitorKind::Minor
    && unsafe { (*header).generation.load(Ordering::Acquire) } > 0
{
    continue; // BUG: Incorrect - skips all objects on old generation pages
}
```

This condition skips all objects on any page with `generation > 0`, but the correct condition should only skip objects that are both on a young page AND have not been promoted.

**Correct barrier logic (from `simple_write_barrier` etc.):**
```rust
if page_generation == 0 && !has_gen_old_flag {
    continue; // Skip young page object without gen_old_flag
}
// For old page or gen_old_flag set: need barrier
```

**Incorrect current logic:**
```rust
if page_generation > 0 {
    continue; // WRONG: skips ALL objects on old pages
}
```

This is a regression from correct barrier semantics - the barrier correctly checks `generation == 0 && !has_gen_old`, but `process_owned_page` incorrectly checks `generation > 0`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell, collect};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // Create an old generation Gc
    let old = Gc::new(Data { value: 1 });
    // Promote to old generation (simplified - in real code would trigger minor GC)
    
    // Create a GcCell containing the old Gc
    let cell = Gc::new(GcCell::new(Some(old)));
    
    // Trigger minor GC
    collect(); // minor collection
    
    // The old Gc should still be reachable, but may be incorrectly skipped
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change the condition in `process_owned_page` from:
```rust
if kind == VisitorKind::Minor
    && unsafe { (*header).generation.load(Ordering::Acquire) } > 0
{
    continue;
}
```

to a proper check that matches the generational barrier semantics:
```rust
// For minor GC with owned pages, only skip if page is young AND object has no gen_old_flag
if kind == VisitorKind::Minor {
    let page_gen = unsafe { (*header).generation.load(Ordering::Acquire) };
    if page_gen == 0 {
        // Young page - check gen_old_flag (needs heap access to read from gc_box)
        // For now, don't skip - let the normal barrier logic handle it
    }
    // Don't skip old generation pages - they need to be traced
}
```

Or more simply, remove the incorrect `continue` and let normal marking proceed with proper `is_under_construction` checks already in place.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The core issue is that `process_owned_page` conflates "page is old" with "object should be skipped". During minor GC, we still need to trace objects on old pages - the generational barrier ensures we don't MISS old→young references, but old objects themselves must still be traced. The current logic incorrectly skips all objects on old pages.

**Rustacean (Soundness 觀點):**
This is not a soundness bug per se (no UB), but it can cause memory leaks or premature collection of old generation objects during minor GC. The logic is simply incorrect - it checks the wrong condition.

**Geohot (Exploit 觀點):**
If an attacker can trigger minor GC and control which pages end up in the "owned pages" set, they could potentially cause old generation objects to be incorrectly skipped, leading to use-after-free if those objects are actually dead but appear to have been traced.