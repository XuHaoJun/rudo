# [Bug]: unified_write_barrier TOCTOU - missing second is_allocated check before has_gen_old read

**Status:** Fixed
**Tags:** Verified

## Resolution (2026-03-30)

**Outcome:** Already fixed in `crates/rudo-gc/src/heap.rs` `unified_write_barrier` both paths.

**Verification:** Static review of current code (lines 3125-3175):

- Large object path (lines 3125-3140): Second `is_allocated(0)` check at line 3132 BEFORE reading `has_gen_old` at line 3136
- Small object path (lines 3158-3174): Second `is_allocated(index)` check at line 3164 BEFORE reading `has_gen_old` at line 3170

Applied via commit: `17498ef fix(heap): add second is_allocated check before has_gen_old read in unified_write_barrier`

## Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | `Medium` | Requires concurrent lazy sweep and write barrier |
| **Severity** | `High` | Could cause barrier to be skipped, leading to premature collection |
| **Reproducibility** | `High` | Requires precise timing of slot reuse during lazy sweep |

---

## Affected Component & Environment
- **Component:** `unified_write_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## Bug Description

### Expected Behavior
The barrier should verify slot is still allocated before reading `has_gen_old`, and should not early-exit if the slot was reused.

### Actual Behavior
In `heap.rs` `unified_write_barrier`, **both the large object path and small object path** are missing the second `is_allocated` check before reading `has_gen_old`. The early exit condition fires based on `has_gen_old` from a potentially reused slot.

---

## Root Cause Analysis

**Buggy code in heap.rs `unified_write_barrier` large object path (lines 3117-3126):**

```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*h_ptr).is_allocated(0) {  // LINE 3118 - FIRST CHECK
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// Cache flag to avoid TOCTOU between check and barrier (bug133).
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 3123 - TOCTOU!
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;  // LINE 3125 - EARLY EXIT (BUG!)
}
```

**Buggy code in `unified_write_barrier` small object path (lines 3145-3155):**

```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*h.as_ptr()).is_allocated(index) {  // LINE 3146 - FIRST CHECK
    return;
}
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
// Cache flag to avoid TOCTOU between check and barrier (bug133).
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 3152 - TOCTOU!
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;  // LINE 3154 - EARLY EXIT (BUG!)
}
```

**Compare to the FIXED `incremental_write_barrier` large object path (lines 3219-3227):**

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
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // NOW SAFE
```

**The race scenario:**
1. Thread A: First `is_allocated(index)` passes at line 3146
2. Thread B: Lazy sweep deallocates the slot, marks it as free
3. Thread C: New allocation reuses the slot with fresh GcBox (generation=0, GEN_OLD_FLAG=false)
4. Thread A: Reads `has_gen_old` at line 3152 from the new object (value=false)
5. Thread A: Checks `generation == 0 && !has_gen_old` → TRUE
6. Thread A: Returns early at line 3154, **skipping the barrier**
7. Thread A: Never reaches the third `is_allocated(index)` check at line 3159

---

## Suggested Fix

Apply the same fix pattern from `incremental_write_barrier` (bug462) to `unified_write_barrier`:

For large object path, add second `is_allocated` check before reading `has_gen_old`:

```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*h_ptr).is_allocated(0) {  // FIRST CHECK
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// FIX: Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU
if !(*h_ptr).is_allocated(0) {  // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
```

For small object path, add second `is_allocated` check before reading `has_gen_old`:

```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*h.as_ptr()).is_allocated(index) {  // FIRST CHECK
    return;
}
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
// FIX: Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU
if !(*h.as_ptr()).is_allocated(index) {  // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
```

---

## Internal Discussion Record

**R. Kent Dybvig (GC Perspective):**
The TOCTOU in `unified_write_barrier` is particularly dangerous because it can cause SATB violations. When a generational reference from an old object to a young object is incorrectly skipped, the young object may be collected prematurely during minor GC. This is the same root cause as bug462 in `incremental_write_barrier`.

**Rustacean (Soundness Perspective):**
This is a classic TOCTOU (Time-Of-Check-Time-Of-Use) bug. The slot can be deallocated and reused between the first is_allocated check and reading has_gen_old. When the slot is reused, the new object may have different generation and flag values, leading to incorrect early exit. This is technically undefined behavior in the sense that we're reading from an object that may have been freed and replaced.

**Geohot (Exploit Perspective):**
If an attacker can influence allocation patterns (e.g., via incremental marking fallback causing emergency allocations), they might be able to trigger this race more reliably. The result would be premature collection of objects that are still reachable.

---

## Related Bugs

- bug462: `incremental_write_barrier` small object path TOCTOU - Fixed
- bug457: `incremental_write_barrier` large object path TOCTOU - Fixed
- bug247: Original issue documenting the pattern (read has_gen_old only after is_allocated)
