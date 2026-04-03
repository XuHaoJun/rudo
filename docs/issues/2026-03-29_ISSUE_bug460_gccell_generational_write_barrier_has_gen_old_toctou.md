# [Bug]: GcCell::generational_write_barrier has_gen_old TOCTOU in both paths

**Status:** Fixed
**Tags:** Verified, Fixed

## Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | `Medium` | Requires concurrent lazy sweep and write barrier on small/large object |
| **Severity** | `High` | Could cause barrier to be skipped, leading to premature collection |
| **Reproducibility** | `High` | Requires precise timing of slot reuse during lazy sweep |

---

## Affected Component & Environment
- **Component:** `GcCell::generational_write_barrier` (cell.rs lines 1363-1390 and 1391-1428)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## Bug Description

### Expected Behavior
The barrier should verify slot is still allocated before reading `has_gen_old` and should not early-exit if the slot was reused.

### Actual Behavior
In `cell.rs` `generational_write_barrier`, **both the large object path and the small object path** have a TOCTOU race where `has_gen_old` is read AFTER the first `is_allocated` check but BEFORE the second check. If the slot is swept and reused between checks, the early exit can fire incorrectly.

---

## Root Cause Analysis

### Large Object Path (lines 1374-1389)

```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*header).is_allocated(0) {  // LINE 1375 - FIRST CHECK
    return;
}
// GEN_OLD early-exit: skip if page young AND object has no gen_old_flag
// (bug202, matches unified_write_barrier).
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 1381 - READ has_gen_old (TOCTOU!)
if (*header).generation.load(Ordering::Acquire) == 0 && !has_gen_old {  // LINE 1382
    return;  // LINE 1383 - EARLY EXIT (BUG!)
}
if !(*header).is_allocated(0) {  // LINE 1385 - SECOND CHECK (TOO LATE!)
    return;
}
```

**Race scenario:**
1. Thread A: First `is_allocated(0)` passes at line 1375
2. Thread B: Lazy sweep deallocates the slot, marks it as free
3. Thread C: New allocation reuses the slot with fresh GcBox (generation=0, GEN_OLD_FLAG=false)
4. Thread A: Reads `has_gen_old` at line 1381 from the new object (value=false)
5. Thread A: Checks `generation == 0 && !has_gen_old` → TRUE
6. Thread A: Returns early at line 1383, **skipping the barrier**
7. Thread A: Never reaches the second `is_allocated(0)` check at line 1385

### Small Object Path (lines 1405-1426)

```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*header.as_ptr()).is_allocated(index) {  // LINE 1407 - FIRST CHECK
    return;
}
// GEN_OLD early-exit: skip if page young AND object has no gen_old_flag
// (bug202, matches unified_write_barrier).
let gc_box_addr = (header_page_addr + header_size + index * block_size)
    as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 1414 - READ has_gen_old (TOCTOU!)
if (*header.as_ptr()).generation.load(Ordering::Acquire) == 0
    && !has_gen_old  // LINE 1415
{
    return;  // LINE 1418 - EARLY EXIT (BUG!)
}
// Second is_allocated check - prevents TOCTOU race (bug376)
if !(*header.as_ptr()).is_allocated(index) {  // LINE 1421 - AFTER early exit (TOO LATE!)
    return;
}
```

Same race scenario as above.

---

## Comparison with Fixed Code

The `incremental_write_barrier` large object path was fixed (bug459). The correct pattern:

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot.
if !(*h_ptr).is_allocated(0) {      // FIRST CHECK
    return;
}
// Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU (bug459).
// Must verify slot is still allocated before reading any GcBox fields.
if !(*h_ptr).is_allocated(0) {      // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();   // NOW SAFE TO READ
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
```

---

## Suggested Fix

Apply the same fix to both paths of `generational_write_barrier`:

**Large object path fix:**
```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*header).is_allocated(0) {      // FIRST CHECK
    return;
}
// FIX: Second is_allocated check BEFORE reading has_gen_old
if !(*header).is_allocated(0) {      // SECOND CHECK - BEFORE has_gen_old read
    return;
}
// GEN_OLD early-exit: skip if page young AND object has no gen_old_flag
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// No third check needed - we've already validated slot is allocated
```

**Small object path fix:**
```rust
// Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
if !(*header.as_ptr()).is_allocated(index) {  // FIRST CHECK
    return;
}
// FIX: Second is_allocated check BEFORE reading has_gen_old
if !(*header.as_ptr()).is_allocated(index) {  // SECOND CHECK - BEFORE has_gen_old read
    return;
}
// GEN_OLD early-exit: skip if page young AND object has no gen_old_flag
let gc_box_addr = (header_page_addr + header_size + index * block_size)
    as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// No third check needed - we've already validated slot is allocated
```

---

## Why It Was Missed

1. **Timing-dependent race**: The bug only manifests when lazy sweep reclaims a slot between the first `is_allocated` check and the `has_gen_old` read, which requires precise thread scheduling
2. **Incremental fix incomplete**: When bug459 was fixed for `incremental_write_barrier` large object path, the same fix was not applied to `generational_write_barrier` or to the small object path
3. **Similar comment misleading**: The comment "read has_gen_old_flag only after is_allocated (bug247)" suggests the check was intentional, but it doesn't account for slot reuse between the two checks

---

## Related Bugs

- bug459: `incremental_write_barrier` large object path TOCTOU - Fixed
- bug122: `incremental_write_barrier` small object path TOCTOU - Open (same issue, different function)
