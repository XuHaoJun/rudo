# [Bug]: `scan_page_for_unmarked_refs` reads generation before `is_allocated` check (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## 修復紀錄 (Fix Applied)

**Date:** 2026-04-10
**Fix:** Moved the first `is_allocated` check to BEFORE reading `marked_generation` in `scan_page_for_unmarked_refs` (incremental.rs:1014-1028).

**Code Change:**
- Added first `is_allocated` check BEFORE `marked_generation` read (line 1017)
- If slot is not allocated, clear mark immediately and break
- This matches the `worker_mark_loop` pattern (marker.rs:909)

**Verification:** `./clippy.sh` passes, `./test.sh` passes.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Lazy sweep concurrent with incremental marking could trigger window |
| **Severity (嚴重程度)** | High | UB - reading from potentially deallocated slot |
| **Reproducibility (重現難度)** | Medium | Race condition between mark and is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_unmarked_refs` in `gc/incremental.rs` (lines 1012-1019)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

In `scan_page_for_unmarked_refs`, the code should follow the correct pattern used in `worker_mark_loop` (marker.rs:909-917):
1. Check `is_allocated` FIRST
2. Then read `generation()`

This ensures we never read from a deallocated slot.

### 實際行為 (Actual Behavior)

In `scan_page_for_unmarked_refs` (incremental.rs:1012-1019):

```rust
let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();  // Line 1012
// Read generation after successful mark to detect slot reuse (bug336).
let marked_generation = unsafe { (*gc_box_ptr).generation() };  // Line 1014 - BEFORE is_allocated!

// FIX bug528: Re-check is_allocated to fix TOCTOU with lazy sweep.
if !(*header).is_allocated(i) {  // Line 1019 - AFTER generation read!
```

The generation is read at line 1014 **before** `is_allocated` is checked at line 1019. If the slot is deallocated between these two lines (by lazy sweep), we're reading `generation()` from deallocated memory.

### 對比正確模式 (`worker_mark_loop` in marker.rs:909-917)

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {  // FIRST: check is_allocated
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    let gc_box_ptr = obj.cast_mut();
    // FIX bug427: Capture generation to detect slot reuse.
    let marked_generation = (*gc_box_ptr).generation();  // AFTER: safe to read now
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Timeline of potential race:**

1. `try_mark(i)` succeeds at line 1002 - slot is marked
2. `gc_box_ptr` derived at line 1012
3. `marked_generation` read at line 1014 - reads generation from current slot occupant
4. **Race window**: Lazy sweep could deallocate slot between line 1014 and 1019
5. `is_allocated(i)` checked at line 1019

If lazy sweep deallocates the slot between line 1014 and 1019:
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
// Thread A: Runs scan_page_for_unmarked_refs
// Thread B: Runs lazy sweep concurrently

// Window: between line 1014 (read generation) and line 1019 (check is_allocated)
// If sweep deallocates slot in this window, we read from deallocated memory
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Reorder the checks to match `worker_mark_loop`:

```rust
Ok(true) => {
    #[allow(clippy::cast_ptr_alignment)]
    #[allow(clippy::unnecessary_cast)]
    #[allow(clippy::ptr_as_ptr)]
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    
    // FIX bug561: Check is_allocated BEFORE reading generation.
    // Must verify slot is still allocated before reading any GcBox fields.
    // Matches worker_mark_loop (marker.rs:909) pattern.
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        break;
    }
    
    // Now safe to read generation from guaranteed allocated slot
    let marked_generation = unsafe { (*gc_box_ptr).generation() };

    // FIX bug528: Re-check is_allocated to fix TOCTOU with lazy sweep.
    if !(*header).is_allocated(i) {
        let current_generation = unsafe { (*gc_box_ptr).generation() };
        if current_generation != marked_generation {
            break;
        }
        (*header).clear_mark_atomic(i);
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

- bug528: scan_page_for_unmarked_refs clears mark on swept-but-not-reused slot (fixed)
- bug557: mark_and_trace_incremental reads gen before is_allocated (fixed)
- marker.rs worker_mark_loop: correct pattern (check is_allocated FIRST)