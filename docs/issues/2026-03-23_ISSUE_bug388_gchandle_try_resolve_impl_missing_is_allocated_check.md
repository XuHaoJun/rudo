# [Bug]: GcHandle::try_resolve_impl missing is_allocated check before dereference (type confusion risk)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires slot to be swept and reused between entry and flag check |
| **Severity (嚴重程度)** | `Critical` | Type confusion - reads flags from wrong object; could cause incorrect behavior or UB |
| **Reproducibility (復現難度)** | `High` | Race condition - requires precise timing between slot sweep and try_resolve_impl |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::try_resolve_impl` (handles/cross_thread.rs:353-406)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.17`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`try_resolve_impl()` should check `is_allocated` BEFORE dereferencing the pointer, similar to `resolve_impl()`. This prevents reading flags from a swept-and-reused slot (type confusion).

### 實際行為 (Actual Behavior)
`try_resolve_impl()` dereferences `self.ptr.as_ptr()` at line 355 BEFORE any `is_allocated` check:

```rust
// Line 355: Dereference FIRST (no is_allocated check!)
let gc_box = &*self.ptr.as_ptr();

// Lines 356-361: Check flags on potentially wrong object
if gc_box.is_under_construction()
    || gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
{
    return None;
}

// is_allocated check comes AFTER dereference (lines 366-371)
```

If the slot was swept and reused, `gc_box` would point to a different object, and we would read `is_under_construction()`, `has_dead_flag()`, and `dropping_state()` from the wrong object.

**Contrast with `resolve_impl()`** (lines 218-226):
```rust
// resolve_impl CORRECTLY checks is_allocated BEFORE dereferencing:
// FIX bug382: Check is_allocated BEFORE dereferencing to avoid TOCTOU.
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "GcHandle::resolve: object slot was swept before dereference"
    );
}

let gc_box = &*self.ptr.as_ptr();  // Dereference AFTER is_allocated check
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

When `GcHandle::try_resolve_impl()` is called:

1. Thread A enters `try_resolve_impl()`
2. Thread B: Slot is swept and reused (new object with different flags)
3. Thread A: Dereferences pointer at line 355, getting `gc_box` for the NEW object
4. Thread A: Checks flags on the NEW object (lines 356-361)
5. Thread A: Checks `is_allocated` at lines 366-371 - may pass or fail depending on new object's state

The bug is that step 3-4 operate on the wrong object. If the new object happens to have `is_under_construction() = false`, `has_dead_flag() = false`, and `dropping_state() = 0`, the function would proceed to call `inc_ref()` on the new object (line 378), corrupting its reference count.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudocode - actual PoC requires careful timing
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct OldObject { value: i32 }

#[derive(Trace)]
struct NewObject { value: i32 }

// Stress test: race slot sweep with GcHandle::try_resolve
// Requires:
// 1. Gc pointing to OldObject at slot S
// 2. OldObject is collected (slot S swept)
// 3. NewObject allocated at slot S
// 4. try_resolve reads NewObject's flags instead of OldObject's
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check BEFORE dereferencing in `try_resolve_impl()`, matching `resolve_impl()`:

```rust
fn try_resolve_impl(&self) -> Option<Gc<T>> {
    unsafe {
        // FIX bug388: Check is_allocated BEFORE dereferencing to avoid type confusion.
        // If slot is swept and reused, we'd read flags from the wrong object.
        if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                return None;
            }
        }

        let gc_box = &*self.ptr.as_ptr();
        // ... rest of function
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic TOCTOU vulnerability in the handle resolution path. The `resolve_impl` was already fixed (bug382) to check `is_allocated` before dereferencing, but `try_resolve_impl` was missed. Dereferencing a swept-and-reused slot causes type confusion where we operate on the wrong object's metadata.

**Rustacean (Soundness 觀點):**
This is a soundness issue. Reading fields (is_under_construction, has_dead_flag, dropping_state) from a different object could lead to incorrect control flow. If the new object happens to have "safe" flag values, the function proceeds and calls `inc_ref()` on the wrong object, corrupting its ref count.

**Geohot (Exploit 觀點):**
The race window is between entry and the is_allocated check. If an attacker can control the allocation pattern (e.g., via spray), they could potentially manipulate which object ends up at the swept slot to have favorable flag values, potentially leading to ref count corruption.

---

## 驗證記錄

**驗證日期:** 2026-03-23
**驗證人員:** opencode

### 驗證結果

Confirmed the discrepancy between `resolve_impl` (has is_allocated check before dereference - bug382 fix) and `try_resolve_impl` (MISSING is_allocated check before dereference).

- `resolve_impl`: Lines 218-224 check `is_allocated` before line 226 dereference
- `try_resolve_impl`: Line 355 dereferences directly, no prior `is_allocated` check

**Status: Open** - Needs fix.

---

## Resolution (2026-03-23)

**Outcome:** Fixed.

Applied fix to `handles/cross_thread.rs` in `try_resolve_impl()`:
- Added `is_allocated` check BEFORE dereferencing (lines 355-361 in fixed code)
- Matches the pattern used in `resolve_impl()` (bug382 fix)
- Clippy passes

```rust
// FIX bug388: Check is_allocated BEFORE dereferencing to avoid type confusion.
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return None;
    }
}

let gc_box = &*self.ptr.as_ptr();
```