# [Bug]: GcBoxWeakRef::upgrade missing is_allocated check after successful CAS path

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires slot to be swept and reused between try_inc_ref_from_zero CAS success and is_allocated check, with ref_count > 0 path (rare) |
| **Severity (嚴重程度)** | High | Slot reuse after successful inc_ref from zero path leads to type confusion (wrong ref_count modified) |
| **Reproducibility (復現難度)** | Very High | Extremely tight race window; difficult to reproduce consistently |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x (all versions with GcBoxWeakRef)

---

## 📝 問題描述 (Description)

In `GcBoxWeakRef::upgrade`, the successful CAS path (`try_inc_ref_from_zero` returned true, lines 719-752) is missing an `is_allocated` check after the generation check. This contrasts with:

1. The `ref_count > 0` path (lines 760-779) which has `is_allocated` check AFTER generation check
2. The `try_upgrade` function's successful CAS path (lines 955-991) which has `is_allocated` check AFTER generation check
3. All other upgrade/cross-thread handle resolve paths which consistently check `is_allocated` after successful inc_ref

### 預期行為 (Expected Behavior)

After a successful `try_inc_ref_from_zero` CAS that transitions `ref_count` from 0 to 1 (resurrection), the code should:
1. Re-check state flags (dropping_state, dead_flag)
2. Verify generation hasn't changed
3. Verify the slot is still allocated via `is_allocated`
4. Only then return `Some(Gc {...})`

### 實際行為 (Actual Behavior)

After successful resurrection CAS, the code only checks state flags and generation, but skips `is_allocated` verification. This means if the slot is swept and reused between the CAS and return, the function returns a `Gc` pointing to the new object in the reused slot. This is a type confusion vulnerability where `GcBox::inc_ref` was called on the wrong object.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The code at lines 740-747 shows:
```rust
// Check is_allocated after successful upgrade to prevent slot reuse issues
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // Don't call dec_ref - slot may be reused (bug133)
        return None;
    }
}
return Some(Gc { ... });
```

But this check is **absent** in the successful CAS path at lines 719-752:
```rust
if gc_box.try_inc_ref_from_zero() {
    let post_resurrection_generation = gc_box.generation();

    // Second check: verify object wasn't dropped between check and CAS
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        let _ = gc_box;  // NOTE: useless binding
        crate::ptr::GcBox::undo_inc_ref(ptr.as_ptr());
        return None;
    }
    // Verify generation hasn't changed
    if post_resurrection_generation != pre_resurrection_generation {
        crate::ptr::GcBox::undo_inc_ref(ptr.as_ptr());
        return None;
    }
    // ⚠️ MISSING: is_allocated check here!
    return Some(Gc { ... });  // ← Returns Gc without verifying slot still allocated
}
```

The consistency issue: every other inc_ref path in the codebase checks `is_allocated` AFTER successful inc_ref. The resurrection path skips this check.

### Why `is_allocated` is needed even after generation check

Even though generation increments on slot reuse (bug347), there's a subtle race:
1. Slot A has `ref_count=0`, `generation=5`, not allocated
2. Thread 1: `try_inc_ref_from_zero()` succeeds (CAS 0→1)
3. Thread 2: Sweep reclaims Slot A, marks it unallocated, allocates new object B in Slot A with `generation=6`
4. Thread 1: Returns `Gc` pointing to Slot A without `is_allocated` check
5. **Type confusion**: `Gc` from Thread 1's return has pointer to Slot A, but object's `generation=6` (not 5), and it's a completely different type

The generation check at line 736 catches slot REUSE (new object in same slot with incremented generation), but without `is_allocated` check, we don't verify the slot is still valid for use.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This bug is a TOCTOU race - PoC requires extreme timing control
// Likely only reproducible with custom test infrastructure or Miri

use rudo_gc::{Gc, Trace, collect_full};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

#[derive(Trace)]
struct A { id: usize }
#[derive(Trace)]
struct B { id: usize }

// Hypothetical scenario requiring:
// 1. Create Gc<A> and drop it (ref_count=0, not yet swept)
// 2. Concurrent thread triggers GC sweep
// 3. Slot reused for Gc<B>
// 4. Original thread's Weak::upgrade races to succeed CAS
// 5. Without is_allocated check, returns Gc pointing to B (type confusion!)
```

**Note**: This bug is extremely difficult to reproduce reliably in practice due to the tight race window. Static analysis and code consistency review is the primary detection method.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check after successful `try_inc_ref_from_zero` CAS, consistent with all other inc_ref paths:

```rust
if gc_box.try_inc_ref_from_zero() {
    let post_resurrection_generation = gc_box.generation();

    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        let _ = gc_box;
        crate::ptr::GcBox::undo_inc_ref(ptr.as_ptr());
        return None;
    }
    if post_resurrection_generation != pre_resurrection_generation {
        crate::ptr::GcBox::undo_inc_ref(ptr.as_ptr());
        return None;
    }
    // ADD: is_allocated check after successful resurrection
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            crate::ptr::GcBox::undo_inc_ref(ptr.as_ptr());
            return None;
        }
    }
    return Some(Gc {
        ptr: AtomicNullable::new(ptr),
        _marker: PhantomData,
    });
}
```

Also remove the useless `let _ = gc_box;` binding at line 729 - it's there to prevent compiler warning but serves no purpose.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is the primary defense against slot reuse in this code path. However, generation alone is insufficient because it requires the sweeper to increment generation when reusing a slot. If the sweeper doesn't properly increment generation (or does so in a way that's visible to the racing thread), the generation check fails to catch the reuse. The `is_allocated` check provides defense-in-depth by directly verifying the slot's allocation status, independent of generation tracking. This is especially important during concurrent GC when multiple threads might be operating on the same heap region.

**Rustacean (Soundness 觀點):**
The `let _ = gc_box;` binding at line 729 is a code smell - it exists only to silence a compiler warning about an unused borrow. This is concerning because it suggests the programmer wasn't thinking clearly about the borrow's purpose. The borrow of `gc_box` serves no purpose in the subsequent `undo_inc_ref` call which uses `ptr.as_ptr()` directly. This pattern appears twice in the file (lines 729 and 968), suggesting a copy-paste origin. While not unsound by itself, it indicates code that hasn't been carefully reviewed.

**Geohot (Exploit 觀點):**
This is a classic type confusion vulnerability. If exploitable, an attacker could:
1. Cause a slot to be swept and reused for a different type
2. Race the weak reference upgrade to succeed before the slot is marked unallocated
3. Obtain a `Gc<T>` pointer to the new object, but with wrong type semantics
4. Potentially achieve arbitrary memory read/write if the attacker controls the new object's layout

The race window is small but real. With careful timing (e.g., using `std::thread::yield_now()` or timing attacks), an attacker might reliably trigger this. The absence of `is_allocated` check in this path while present everywhere else suggests this code path hasn't received the same security scrutiny.

---

## 驗證清單 (Verification Checklist)

- [ ] Confirm `is_allocated` check is present in `try_inc_ref_from_zero` success path
- [ ] Confirm `is_allocated` check is present in `try_upgrade` success path  
- [ ] Remove useless `let _ = gc_box;` binding
- [ ] Verify all other `inc_ref`/`try_inc_ref_*` paths have `is_allocated` after successful increment
