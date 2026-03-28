# [Bug]: GcBoxWeakRef::clone inc_weak called before is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires slot to be swept and reused between validity checks and inc_weak |
| **Severity (嚴重程度)** | `Medium` | Corrupts weak count of wrong object; can lead to premature reclamation or leaks |
| **Reproducibility (復現難度)** | `High` | Race condition - difficult to reproduce reliably without stress testing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::clone` (ptr.rs:729-785)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcBoxWeakRef::clone()` should validate the slot is still allocated BEFORE calling `inc_weak()`, preventing corruption of another object's weak count.

### 實際行為 (Actual Behavior)
`inc_weak()` is called at line 770 **before** the `is_allocated` check at line 772. If the slot is swept and reused between the validity checks (lines 738-768) and the `inc_weak` call, `inc_weak` operates on the new object at that slot.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `GcBoxWeakRef::clone()` (ptr.rs:729-785):

```rust
// Lines 752-768: Validity checks on old object
unsafe {
    let gc_box = &*ptr.as_ptr();
    if gc_box.has_dead_flag() { return null; }
    if gc_box.dropping_state() != 0 { return null; }
    
    (*ptr.as_ptr()).inc_weak();  // LINE 770 - inc_weak called HERE
    
    // Lines 772-779: is_allocated check AFTER inc_weak
    if let Some(idx) = crate::heap::ptr_to_object_index(...) {
        let header = crate::heap::ptr_to_page_header(...);
        if !(*header.as_ptr()).is_allocated(idx) {
            return Self { ptr: AtomicNullable::null() };  // Returns null but weak_count corrupted
        }
    }
}
```

**Race scenario:**
1. Thread A: Validates old object (passes all checks)
2. Thread B: Slot is swept, new object allocated at same address
3. Thread A: Calls `inc_weak()` on new object (corrupting its weak count)
4. Thread A: Checks `is_allocated` - returns null

The function returns null but the weak count of the new object has been incorrectly incremented.

**Contrast with `GcHandle::downgrade()`** (cross_thread.rs:411-440):
```rust
// Get generation BEFORE inc_weak to detect slot reuse (bug351).
let pre_generation = (*self.ptr.as_ptr()).generation();
(*self.ptr.as_ptr()).inc_weak();
// Verify generation hasn't changed
if pre_generation != (*self.ptr.as_ptr()).generation() {
    (*self.ptr.as_ptr()).dec_weak();
    return WeakCrossThreadHandle { weak: GcBoxWeakRef::null(), ... };
}
if let Some(idx) = ... {
    let header = crate::heap::ptr_to_page_header(...);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*self.ptr.as_ptr()).dec_weak();
        return WeakCrossThreadHandle { weak: GcBoxWeakRef::null(), ... };
    }
}
```

`GcHandle::downgrade()` checks `is_allocated` BEFORE `inc_weak` and has generation checking. `GcBoxWeakRef::clone()` does neither properly.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudocode - actual PoC requires careful timing
use rudo_gc::{Gc, Weak, Trace};

#[derive(Trace)]
struct Data { value: i32 }

let gc = Gc::new(Data { value: 42 });
let weak = Gc::downgrade(&gc);

// Stress test: race slot sweep with weak.clone()
// Requires concurrent GC activity to sweep the slot
// while clone() is between validity checks and inc_weak

// The bug manifests as:
// 1. New object's weak_count incorrectly incremented
// 2. New object may be prematurely collected OR leak
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the `is_allocated` check BEFORE the `inc_weak()` call, similar to `GcHandle::downgrade()`:

```rust
unsafe {
    let gc_box = &*ptr.as_ptr();
    
    // Check is_allocated BEFORE inc_weak
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return Self { ptr: AtomicNullable::null() };
        }
    }
    
    (*ptr.as_ptr()).inc_weak();
}
```

Also consider adding generation checking like `GcHandle::downgrade()` does for bug351.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The bug is a classic TOCTOU (time-of-check-time-of-use) issue in the weak reference cloning path. The `inc_weak` being called before `is_allocated` verification allows a window where the slot could be reused. The correct pattern (as seen in `GcHandle::downgrade`) checks allocation status before modifying reference counts. This corrupts the weak count metadata, which can lead to either premature collection or memory leaks depending on the race outcome.

**Rustacean (Soundness 觀點):**
While this doesn't cause immediate UB (the function eventually returns null), it corrupts internal GC state (weak_count of a live object). This is a violation of the GC's internal invariants and could lead to subtle memory safety issues over time. The pattern of checking validity before use is not followed here.

**Geohot (Exploit 觀點):**
The TOCTOU window is relatively small (between validity checks and inc_weak), but in a concurrent GC environment with stress testing, this window can be exploited. Corrupting weak_count could potentially be leveraged for use-after-free if an object is prematurely collected while a legitimate weak reference exists.

---

## Resolution (2026-03-20)

**Outcome:** Code Fixed, Comment Still Wrong.

Applied fix in `ptr.rs:729-798` following the pattern from `GcHandle::downgrade()`:

1. Get `pre_generation` before `inc_weak()` to detect slot reuse
2. Call `inc_weak()`
3. Verify generation hasn't changed - if changed, undo with `dec_weak()` and return null
4. Check `is_allocated` - if slot was swept, undo with `dec_weak()` and return null

This prevents corrupting another object's weak_count when the slot is swept and reused between validity checks and `inc_weak()`.

## Follow-up (2026-03-24): Misleading comment — resolved (2026-03-28)

The outdated "BEFORE inc_weak" wording was removed from `GcBoxWeakRef::clone`; comments now state that `is_allocated` runs **after** `inc_weak`, with the generation check as the primary slot-reuse guard. A stale line-number reference in that comment block was replaced with "above" to avoid drift.

**Status:** (sub-issue closed; main issue remains **Fixed** / **Verified**)