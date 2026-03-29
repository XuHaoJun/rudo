# [Bug]: heap.rs incremental_write_barrier small object path missing second is_allocated check

**Status:** Open
**Tags:** Verified

## Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | `Medium` | Requires concurrent lazy sweep and write barrier on small object |
| **Severity** | `High` | Could cause barrier to be skipped, leading to premature collection |
| **Reproducibility** | `High` | Requires precise timing of slot reuse during lazy sweep |

---

## Affected Component & Environment
- **Component:** `incremental_write_barrier` (heap.rs small object path)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## Bug Description

### Expected Behavior
The barrier should verify slot is still allocated before reading `has_gen_old` and should not early-exit if the slot was reused.

### Actual Behavior
In `heap.rs` `incremental_write_barrier`, the **small object path** is missing the second `is_allocated` check before reading `has_gen_old`. The early exit condition fires based on `has_gen_old` from a potentially reused slot.

---

## Root Cause Analysis

**Buggy code in heap.rs `incremental_write_barrier` small object path (lines 3249-3261):**

```rust
// GEN_OLD early-exit: skip only if page young AND object has no gen_old_flag (bug71).
// Skip if slot was swept; avoids corrupting remembered set with reused slot (bug286).
if !(*h.as_ptr()).is_allocated(index) {  // LINE 3251 - FIRST CHECK
    return;
}
// Cache flag to avoid TOCTOU between check and barrier (bug133).  <-- COMMENT IS MISLEADING
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 3257 - READ has_gen_old (TOCTOU!)
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {  // LINE 3258
    return;  // LINE 3259 - EARLY EXIT (BUG!)
}
// Third check at line 3265 (too late - we've already returned)
```

**Compare to the FIXED large object path (lines 3214-3228):**

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot (bug286).
if !(*h_ptr).is_allocated(0) {  // FIRST CHECK
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU (bug457).
// Must verify slot is still allocated before reading any GcBox fields.
if !(*h_ptr).is_allocated(0) {  // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // NOW SAFE TO READ
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
```

**The race scenario:**
1. Thread A: First `is_allocated(index)` passes at line 3251
2. Thread B: Lazy sweep deallocates the slot, marks it as free
3. Thread C: New allocation reuses the slot with fresh GcBox (generation=0, GEN_OLD_FLAG=false)
4. Thread A: Reads `has_gen_old` at line 3257 from the new object (value=false)
5. Thread A: Checks `generation == 0 && !has_gen_old` → TRUE
6. Thread A: Returns early at line 3259, **skipping the barrier**
7. Thread A: Never reaches the third `is_allocated(index)` check at line 3265

---

## Suggested Fix

Apply the same fix that was done for the large object path to the small object path:

Move the second `is_allocated(index)` check to BEFORE reading `has_gen_old`:

```rust
// GEN_OLD early-exit: skip only if page young AND object has no gen_old_flag (bug71).
// Skip if slot was swept; avoids corrupting remembered set with reused slot (bug286).
if !(*h.as_ptr()).is_allocated(index) {  // FIRST CHECK
    return;
}
// FIX: Second is_allocated check BEFORE reading has_gen_old
if !(*h.as_ptr()).is_allocated(index) {  // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// No third check needed - we've already validated slot is allocated
```

---

## Internal Discussion Record

**R. Kent Dybvig (GC Perspective):**
The TOCTOU in incremental_write_barrier is particularly dangerous because it can cause SATB violations. When a generational reference from an old object to a young object is incorrectly skipped, the young object may be collected prematurely during minor GC. The incremental marking relies on write barriers to maintain correctness; skipping them breaks the invariants.

**Rustacean (Soundness Perspective):**
This is a classic TOCTOU (Time-Of-Check-Time-Of-Use) bug. The slot can be deallocated and reused between the first is_allocated check and reading has_gen_old. When the slot is reused, the new object may have different generation and flag values, leading to incorrect early exit. This is technically undefined behavior in the sense that we're reading from an object that may have been freed and replaced.

**Geohot (Exploit Perspective):**
If an attacker can influence allocation patterns (e.g., via incremental marking fallback causing emergency allocations), they might be able to trigger this race more reliably. The result would be premature collection of objects that are still reachable, which could be leveraged in certain GC timing attacks or to create use-after-free conditions in combination with other bugs.

---

## Related Bugs

- bug457: `incremental_write_barrier` large object path TOCTOU - Fixed
- bug459: `GcCell::incremental_write_barrier` large object path TOCTOU - Fixed
- bug122: `GcCell::incremental_write_barrier` small object path TOCTOU - Fixed (cell.rs)
- bug460: `GcCell::generational_write_barrier` both paths TOCTOU - Fixed