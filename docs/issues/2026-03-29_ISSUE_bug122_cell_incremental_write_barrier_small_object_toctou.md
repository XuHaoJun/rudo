# [Bug]: GcCell::incremental_write_barrier small object path has_gen_old TOCTOU

**Status:** Open
**Tags:** Unverified

## 📊 Threat Model Assessment

| Assessment | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | `Medium` | Requires concurrent lazy sweep and write barrier on small object |
| **Severity** | `High` | Could cause barrier to be skipped, leading to premature collection |
| **Reproducibility** | `High` | Requires precise timing of slot reuse during lazy sweep |

---

## 🩹 Affected Component & Environment
- **Component:** `GcCell::incremental_write_barrier` (cell.rs small object path)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## 📝 Bug Description

### Expected Behavior
The barrier should verify slot is still allocated before reading `has_gen_old` and should not early-exit if the slot was reused.

### Actual Behavior
In `cell.rs` `incremental_write_barrier`, the **small object path** has a TOCTOU race where `has_gen_old` is read at line 1335 BEFORE the second `is_allocated` check at line 1340. The early exit condition at lines 1336-1337 fires based on `has_gen_old` from a potentially reused slot.

---

## 🔬 Root Cause Analysis

**Buggy code in cell.rs `incremental_write_barrier` small object path (lines 1329-1343):**

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot.
if !(*h.as_ptr()).is_allocated(index) {  // LINE 1330 - FIRST CHECK
    return;
}
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 1335 - READ has_gen_old
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {  // LINE 1336
    return;  // LINE 1337 - EARLY EXIT (BUG!)
}
// Second is_allocated check - prevents TOCTOU race (bug376)
if !(*h.as_ptr()).is_allocated(index) {  // LINE 1340 - AFTER early exit (TOO LATE!)
    return;
}
```

**Compare to heap.rs `incremental_write_barrier` (which was fixed in commit e320eb5):**

```rust
if !(*h_ptr).is_allocated(0) {      // FIRST CHECK
    return;
}
if !(*h_ptr).is_allocated(0) {      // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();   // NOW SAFE TO READ
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
```

**The race scenario:**
1. Thread A: First `is_allocated(index)` passes at line 1330
2. Thread B: Lazy sweep deallocates the slot, marks it as free
3. Thread C: New allocation reuses the slot with fresh GcBox (generation=0, GEN_OLD_FLAG=false)
4. Thread A: Reads `has_gen_old` at line 1335 from the new object (value=false)
5. Thread A: Checks `generation == 0 && !has_gen_old` → TRUE
6. Thread A: Returns early at line 1337, **skipping the barrier**
7. Thread A: Never reaches the second `is_allocated(index)` check at line 1340

**Note:** This is a race condition that requires precise timing.

---

## 🛠️ Suggested Fix

Apply the same fix that was done in heap.rs (commit e320eb5) to the cell.rs `incremental_write_barrier` small object path:

Move the second `is_allocated(index)` check to BEFORE reading `has_gen_old`:

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot.
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

## Additional Issues Found

The `GcCell::generational_write_barrier` function has the SAME TOCTOU bug in BOTH paths:

**Large object path (lines 1374-1387):**
- Line 1375: First `is_allocated(0)` check
- Line 1381: `has_gen_old` is read (TOCTOU window!)
- Line 1382-1383: Early exit using has_gen_old
- Line 1385: Second `is_allocated(0)` check AFTER has_gen_old was read

**Small object path (lines 1406-1423):**
- Line 1407: First `is_allocated(index)` check
- Line 1414: `has_gen_old` is read (TOCTOU window!)
- Line 1415-1418: Early exit using has_gen_old
- Line 1421: Second `is_allocated(index)` check AFTER has_gen_old was read

---

## Related Bugs

- bug459: Same issue in large object path (cell.rs `incremental_write_barrier` large object path) - Open
- bug457: Same issue in heap.rs `incremental_write_barrier` large object path - Fixed
- bug376: GcThreadSafeCell barrier TOCTOU - related but different function
- bug282: Similar issue in heap.rs `incremental_write_barrier` - Invalid (but bug457 is the fix for that)
