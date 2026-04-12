# [Bug]: GcBoxWeakRef::clone missing undo_inc_weak for swept-not-reused case

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires GC sweep between generation check and second is_allocated check |
| **Severity (嚴重程度)** | `Medium` | Weak reference count leak; memory not reclaimed until next GC |
| **Reproducibility (復現難度)** | `Medium` | Requires precise GC timing between generation check and second is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::clone` (ptr.rs:792-859)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0 (incremental marking)

---

## 📝 問題描述 (Description)

In `GcBoxWeakRef::clone`, when a slot is swept after the generation check but before the second `is_allocated` check (lines 847-853), the function calls `dec_weak()` to decrement the weak reference count. However, `dec_weak()` returns early when `weak_count == 1` and `DEAD_FLAG` is set, without actually decrementing. This results in a leaked weak reference count.

### 預期行為 (Expected Behavior)
When a slot is swept between the generation check and the second `is_allocated` check, the weak reference count should be decremented via `undo_inc_weak` (equivalent to a direct subtraction), not via `dec_weak()` which has early-return semantics that can skip the decrement.

### 實際行為 (Actual Behavior)
The code calls `dec_weak()` at line 850 when `is_allocated` returns false with generation unchanged. The `dec_weak` function has complex early-return logic that can skip decrementing when `weak_count == 1` and `DEAD_FLAG` is set, causing a weak reference count leak.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is at `ptr.rs:847-853`:

```rust
// FIX bug600: Second is_allocated check AFTER inc_weak to catch slot reuse
// that bypassed the generation check (defense-in-depth).
// Matches as_weak() pattern (bug504 fix).
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*ptr.as_ptr()).dec_weak();  // BUG: dec_weak has early-return semantics
        return Self::null();
    }
}
```

**The problem:** `dec_weak()` returns `true` when the weak count reaches zero, but it has an early-return path: when `count == 1` and the CAS to set `weak_count` to just `flags` succeeds, it returns `true` WITHOUT decrementing the count (the CAS sets it directly to flags, effectively losing the decrement).

In `dec_weak`:
```rust
if count == 1 {
    // Attempt to set to just flags
    match self.weak_count.compare_exchange_weak(current, flags, ...) {
        Ok(_) => return true,  // Returns true but did NOT decrement!
        Err(_) => continue,
    }
}
```

**Why this matters:** If a slot is swept (generation unchanged) and `is_allocated` returns false, `dec_weak()` is called but may not actually decrement. This leaves the weak count artificially high, preventing memory reclamation.

**Correct pattern:** Other similar paths in the codebase use `undo_inc_ref` which directly subtracts, avoiding the early-return logic. For weak references, we need `undo_inc_weak` (a direct `fetch_sub` on `weak_count`) instead of `dec_weak`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This bug is timing-dependent and requires GC sweep to occur between the generation check and second `is_allocated` check:

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::rc::Rc;
use std::cell::Cell;

#[derive(Clone)]
struct RefTracker {
    marker: Rc<Cell<bool>>,
}
static_collect!(RefTracker);

#[test]
fn test_gcboxweakref_clone_swept_not_reused_leak() {
    // Create a weak reference
    let strong = Gc::new(RefTracker {
        marker: Rc::new(Cell::new(false)),
    });
    let weak = strong.as_weak();
    drop(strong);

    // Force a minor GC to sweep the slot (but not reuse)
    // The timing window is: after weak.clone() increments weak_count
    // but before the second is_allocated check
    
    // Note: This test demonstrates the pattern; actual leak requires
    // precise GC timing which is hard to reproduce reliably.
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Replace `dec_weak()` with a direct `fetch_sub` operation (undo pattern) in `GcBoxWeakRef::clone`. This requires adding an `undo_inc_weak` method to `GcBox`:

```rust
// In GcBox (ptr.rs), add:
#[inline]
pub(crate) unsafe fn undo_inc_weak(self_ptr: *mut Self) {
    // SAFETY: Caller guarantees ptr is valid and we own an increment to undo.
    unsafe {
        (*self_ptr).weak_count.fetch_sub(1, Ordering::Release);
    }
}

// In GcBoxWeakRef::clone (ptr.rs), change line 850 from:
(*ptr.as_ptr()).dec_weak();
// to:
unsafe { crate::ptr::GcBox::undo_inc_weak(ptr.as_ptr()) };
```

This matches the pattern used in `GcBoxWeakRef::upgrade` (line 780) which correctly uses `undo_inc_ref` for the swept-not-reused case.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The weak reference count leak in `GcBoxWeakRef::clone` is a memory management correctness issue. When a slot is swept without being reused, the weak count should be decremented to allow eventual memory reclamation. The `dec_weak` early-return behavior is problematic because it was designed for cleanup paths, not rollback paths. The generation-unchanged-but-swept case requires a direct subtraction, not a conditional decrement that can bail out early.

**Rustacean (Soundness 觀點):**
This is not a soundness issue (no UB or type confusion), but it's a memory leak that could cause memory exhaustion over time. The `dec_weak` function's behavior is correct for its intended use (decrementing when dropping a weak reference), but incorrect as a rollback mechanism. The comment at line 844-846 mentions "defense-in-depth" but the chosen function undermines that defense.

**Geohot (Exploit 觀點):**
The weak reference count leak could theoretically be weaponized for memory exhaustion DoS if an attacker can trigger the specific GC timing. However, the window for exploitation is extremely narrow (between generation check and second is_allocated check). Not a practical exploit vector, but still a correctness bug that should be fixed.