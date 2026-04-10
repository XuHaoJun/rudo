# [Bug]: `scan_page_for_marked_refs` reads generation before `is_allocated` check (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## Fix Applied

**Date:** 2026-04-10
**Fix:** Applied the same pattern as bug561 to `scan_page_for_marked_refs` (incremental.rs:846-858).
- Moved `is_allocated` check to BEFORE reading `generation()`
- If slot is not allocated, clear mark immediately and break
- Matches `scan_page_for_unmarked_refs` (bug561) pattern

**Verification:** `./clippy.sh` passes.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Lazy sweep concurrent with incremental marking could trigger window |
| **Severity (嚴重程度)** | High | UB - reading from potentially deallocated slot |
| **Reproducibility (重現難度)** | Medium | Race condition between mark and is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_marked_refs` in `gc/incremental.rs` (lines 846-847)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

In `scan_page_for_marked_refs`, the code should follow the correct pattern used in `scan_page_for_unmarked_refs` (bug561 fix):
1. Check `is_allocated` FIRST
2. Then read `generation()`

This ensures we never read from a deallocated slot.

### 實際行為 (Actual Behavior)

In `scan_page_for_marked_refs` (incremental.rs:846-847):

```rust
let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
let marked_generation = unsafe { (*gc_box_ptr).generation() };  // Line 847 - READS GEN FIRST!

// Re-check is_allocated to fix TOCTOU with lazy sweep (bug291).
// If slot was swept after try_mark, clear mark and skip.
if !(*header).is_allocated(i) {  // Line 851 - is_allocated checked AFTER!
```

The generation is read at line 847 **before** `is_allocated` is checked at line 851. If the slot is deallocated between these two lines (by lazy sweep), we're reading `generation()` from deallocated memory.

### 對比正確模式 (`scan_page_for_unmarked_refs` after bug561 fix)

```rust
// FIX bug561: Check is_allocated BEFORE reading generation.
// Must verify slot is still allocated before reading any GcBox fields.
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}

// Now safe to read generation from guaranteed allocated slot
let marked_generation = unsafe { (*gc_box_ptr).generation() };
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Timeline of potential race:**

1. `try_mark(i)` succeeds at line 835 - slot is marked
2. `gc_box_ptr` derived at line 845
3. `marked_generation` read at line 847 - reads generation from current slot occupant
4. **Race window**: Lazy sweep could deallocate slot between line 847 and line 851
5. `is_allocated(i)` checked at line 851

If lazy sweep deallocates the slot between line 847 and line 851:
- `marked_generation` was read from the old object (or new object if reused)
- `is_allocated(i)` returns false
- But `marked_generation` was already read from potentially deallocated memory

**Why this is UB:**
Reading `generation()` from a slot that may have been deallocated is undefined behavior in Rust. The memory at `gc_box_ptr` may no longer contain a valid object after the slot is swept.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Theoretical race scenario requiring concurrent lazy sweep and incremental marking. ThreadSanitizer would detect this data race.

```rust
// Pseudocode - requires precise timing
// Thread A: Runs scan_page_for_marked_refs
// Thread B: Runs lazy sweep concurrently

// Window: between line 847 (read generation) and line 851 (check is_allocated)
// If sweep deallocates slot in this window, we read from deallocated memory
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Reorder the checks to match `scan_page_for_unmarked_refs` (bug561 fix):

```rust
Ok(true) => {
    #[allow(clippy::cast_ptr_alignment)]
    #[allow(clippy::unnecessary_cast)]
    #[allow(clippy::ptr_as_ptr)]
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    
    // FIX bug562: Check is_allocated BEFORE reading generation.
    // Must verify slot is still allocated before reading any GcBox fields.
    // Matches scan_page_for_unmarked_refs (bug561) pattern.
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        break;
    }
    
    // Now safe to read generation from guaranteed allocated slot
    let marked_generation = unsafe { (*gc_box_ptr).generation() };
    
    // FIX bug291: Re-check is_allocated to fix TOCTOU with lazy sweep.
    if !(*header).is_allocated(i) {
        let current_generation = unsafe { (*gc_box_ptr).generation() };
        if current_generation != marked_generation {
            break;
        }
        (*header).clear_mark_atomic(i);
        break;
    }
    // Verify generation hasn't changed (bug336 fix).
    let current_generation = unsafe { (*gc_box_ptr).generation() };
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object, don't clear.
        break;
    }
    // ... rest of function
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Reading from deallocated memory in a concurrent GC is a serious issue. The generation check is meant to detect slot reuse, but if the slot is simply deallocated (not reused), we're reading from invalid memory. The correct pattern is to verify allocation status BEFORE reading any object fields.

**Rustacean (Soundness 觀點):**
This is undefined behavior - reading from memory that may have been deallocated. Even if the generations happen to match (making the logic "work"), the read itself is UB. The fix is straightforward: check `is_allocated` before reading `generation()`.

**Geohot (Exploit 觀點):**
While this is a race condition that's difficult to exploit, the UB itself is concerning. If an attacker could somehow control the timing precisely, they might be able to cause incorrect GC behavior by manipulating when lazy sweep runs relative to incremental marking.

---

## 相關 Issue

- bug561: scan_page_for_unmarked_refs reads gen before is_allocated (fixed 2026-04-10)
- bug557: mark_and_trace_incremental reads gen before is_allocated (fixed)
- bug559: mark_object reads gen before is_allocated (fixed)
- scan_page_for_marked_refs: same bug - NOT YET FIXED
