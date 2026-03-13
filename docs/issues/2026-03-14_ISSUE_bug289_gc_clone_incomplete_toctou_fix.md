# [Bug]: Gc::clone has incomplete TOCTOU fix - missing is_allocated check BEFORE inc_ref

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent GC sweep during clone operation |
| **Severity (嚴重程度)** | Critical | Memory corruption - wrong object's ref count incremented |
| **Reproducibility (重現難度)** | High | Requires precise timing of concurrent GC sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::clone()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`Gc::clone()` should verify the object slot is still allocated BEFORE incrementing the ref count. This prevents incrementing the wrong object's ref count when the slot is swept and reused between the flag check and `inc_ref()`.

### 實際行為
Bug206 fixed the missing `is_allocated` check AFTER `inc_ref()`, but this fix is incomplete. The current code still has a TOCTOU race:

1. Lines 1773-1778: Check flags (`has_dead_flag`, `dropping_state`, `is_under_construction`)
2. Line 1779: Call `inc_ref()` - **NO is_allocated check before this!**
3. Lines 1781-1788: Check `is_allocated` - **TOO LATE!**

Between step 1 and step 2, the slot could be swept and a NEW object allocated. Then `inc_ref()` would increment the NEW object's ref count, causing:
- Memory corruption (wrong object's ref count modified)
- Use-after-free scenarios
- Potential double-free or leak

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `ptr.rs:1773-1795` (Gc::clone):

```rust
// Step 1: Check flags (lines 1773-1778)
assert!(
    !(*gc_box_ptr).has_dead_flag()
        && (*gc_box_ptr).dropping_state() == 0
        && !(*gc_box_ptr).is_under_construction(),
    "Gc::clone: cannot clone a dead, dropping, or under construction Gc"
);

// Step 2: inc_ref - NO is_allocated check before this!
(*gc_box_ptr).inc_ref();  // LINE 1779

// Step 3: is_allocated check - TOO LATE! (lines 1781-1788)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::clone: object slot was swept after inc_ref"
    );
}
```

The race scenario:
1. Thread A: Checks flags (step 1) - all valid
2. Thread B: Sweeper runs, slot is swept (ref_count = 0), new object allocated in same slot
3. Thread A: Calls `inc_ref()` (step 2) - modifies NEW object's header!
4. Thread A: `is_allocated` check (step 3) - PASSES because new object IS allocated!

Compare with correct pattern in `try_clone()`:
- Uses `try_inc_ref_if_nonzero()` which atomically checks and increments
- Has pre-check for is_allocated before any operation

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // Requires concurrent testing environment
    // 1. Create a Gc object
    // 2. Trigger lazy sweep to reclaim the object
    // 3. Allocate new object in same slot
    // 4. Concurrently call Gc::clone()
    // 5. Observe wrong ref count on new object
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check BEFORE calling `inc_ref()`, similar to how `try_clone` handles it:

```rust
// Add BEFORE line 1779:
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        panic!("Gc::clone: object slot was swept before inc_ref");
    }
}

(*gc_box_ptr).inc_ref();
```

Or better: use atomic check like `try_inc_ref_if_nonzero()` to avoid TOCTOU entirely.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The fix for bug206 added is_allocated check AFTER inc_ref, but this doesn't prevent the race between flag checking and ref increment. The slot could be reused between these operations, causing the increment to target the wrong object. This is a fundamental TOCTOU issue that requires checking BEFORE any state modification.

**Rustacean (Soundness 觀點):**
Incrementing the wrong object's ref count is a serious memory safety issue. It can lead to:
- Premature collection of the new object (ref count artificially low)
- Memory leaks (old object's count artificially high)
- Double-free when both objects are dropped

**Geohot (Exploit 攻擊觀點):**
If an attacker can control allocation timing, they could:
1. Free the original object
2. Quickly allocate a controlled object in the same slot
3. Trigger clone to increment the controlled object's ref count
4. Exploit the corrupted ref count state

---

## 🔗 相關 Issue

- bug206: GcHandle::resolve/clone missing is_allocated check after inc_ref (Fixed - partial fix)
- bug250: Gc::try_clone missing is_allocated check (similar pattern, Open)
- bug271: Ephemeron::key() always returns None (different issue)
