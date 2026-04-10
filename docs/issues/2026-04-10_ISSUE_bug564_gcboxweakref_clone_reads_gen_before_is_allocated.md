# [Bug]: GcBoxWeakRef::clone reads generation before is_allocated check (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Concurrent lazy sweep could deallocate slot between generation read and is_allocated |
| **Severity (嚴重程度)** | High | UB - reading from potentially deallocated slot |
| **Reproducibility (復現難度)** | Medium | Race condition between mark and is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::clone` in `ptr.rs` (lines 818-837)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

Following the correct pattern (as seen in bug561/bug562 fixes):
1. Check `is_allocated` FIRST
2. Then read `generation()`
3. Then call `inc_weak()`

This ensures we never read from a deallocated slot.

### 實際行為 (Actual Behavior)

In `GcBoxWeakRef::clone()` (ptr.rs:818-837):

```rust
// Line 818-819: Reads generation BEFORE is_allocated check
// Get generation BEFORE inc_weak to detect slot reuse (bug354).
let pre_generation = (*ptr.as_ptr()).generation();  // LINE 819 - READS GEN!

(*ptr.as_ptr()).inc_weak();  // Line 821

// Lines 823-827: Generation check (detects REUSE, not deallocation)
if pre_generation != (*ptr.as_ptr()).generation() {
    (*ptr.as_ptr()).dec_weak();
    return Self::null();
}

// Lines 829-837: is_allocated check AFTER inc_weak + generation read
// Check is_allocated AFTER inc_weak + generation check.
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {  // LINE 834 - is_allocated checked LATE!
        (*ptr.as_ptr()).dec_weak();
        return Self::null();
    }
}
```

The generation is read at line 819 **before** `is_allocated` is checked at line 834. If the slot is deallocated (but not reused) between these two lines, we're reading `generation()` from deallocated memory.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Timeline of potential race:**

1. Thread A: Enters `GcBoxWeakRef::clone()`
2. Lines 805-816: Validates gc_box fields (has_dead_flag, dropping_state)
3. Line 819: Reads `pre_generation` from slot
4. **Race window**: Lazy sweep deallocates slot (slot is empty, not reused)
5. Line 821: `inc_weak()` called on deallocated slot
6. Line 824: Generation check passes (generation unchanged - slot is empty, not reused)
7. Line 834: `is_allocated` returns false, but we've already read from freed memory

**Why this is UB:**
- The generation check at line 824 detects **slot REUSE** (new object in slot)
- It does NOT detect **simple deallocation** (slot is empty)
- If slot is deallocated but not reused, `pre_generation` equals `current_generation`
- But `generation()` was read from deallocated memory - this is UB

**Contrast with bug561/bug562 fixes:**
The correct pattern is to check `is_allocated` FIRST, then read any GcBox fields:

```rust
// FIX pattern (bug561/bug562):
if !(*header).is_allocated(idx) {
    return Self::null();
}
// Now safe to read generation
let pre_generation = (*ptr.as_ptr()).generation();
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Theoretical race scenario requiring concurrent lazy sweep and weak clone. Miri or ThreadSanitizer would detect this UB.

```rust
// Pseudocode - requires precise timing
// Thread A: Runs GcBoxWeakRef::clone
// Thread B: Runs lazy sweep to deallocate (not reuse) the slot

// Window: between line 819 (read generation) and line 834 (check is_allocated)
// If sweep deallocates slot (empty, not reused) in this window,
// generation check passes but we read from deallocated memory
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Reorder checks to follow bug561/bug562 pattern:

```rust
unsafe {
    let gc_box = &*ptr.as_ptr();

    if gc_box.has_dead_flag() {
        return Self::null();
    }

    if gc_box.dropping_state() != 0 {
        return Self::null();
    }

    // FIX: Check is_allocated BEFORE reading generation.
    // Must verify slot is still allocated before reading any GcBox fields.
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return Self::null();
        }
    }

    // Now safe to read generation from guaranteed allocated slot
    let pre_generation = (*ptr.as_ptr()).generation();

    (*ptr.as_ptr()).inc_weak();

    // Verify generation hasn't changed - if slot was reused, undo inc_weak.
    if pre_generation != (*ptr.as_ptr()).generation() {
        (*ptr.as_ptr()).dec_weak();
        return Self::null();
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Reading from deallocated memory in a concurrent GC is a serious issue. The generation check was added in bug354 to detect slot REUSE, but it doesn't protect against simple deallocation. The correct pattern is to verify allocation status BEFORE reading any object fields. This is the same fundamental issue as bug561/bug562 but in a different code path.

**Rustacean (Soundness 觀點):**
This is undefined behavior - reading from memory that may have been deallocated. Even if the generations happen to match (making the logic "work" for the reuse case), the read itself is UB when the slot is simply deallocated. The fix is straightforward: check `is_allocated` before reading `generation()`.

**Geohot (Exploit 觀點):**
While this is a race condition that's difficult to exploit, the UB itself is concerning. If an attacker could somehow control the timing precisely, they might be able to cause incorrect GC behavior by manipulating when lazy sweep runs relative to weak reference operations. The deallocated-but-not-reused case is particularly insidious because the generation check doesn't catch it.

---

## 相關 Issue

- bug561: scan_page_for_unmarked_refs reads gen before is_allocated (fixed 2026-04-10)
- bug562: scan_page_for_marked_refs reads gen before is_allocated (fixed 2026-04-10)
- bug354: GcBoxWeakRef::clone inc_weak before is_allocated (fixed 2026-03-20) - partial fix only

---

## 驗證記錄

**驗證日期:** 2026-04-10
**驗證人員:** opencode

### 驗證結果

1. **Code inspection**: Verified the fix follows the bug561/bug562 pattern - `is_allocated` check moved to BEFORE `generation()` read.
2. **Clippy**: `./clippy.sh` passes with no warnings.
3. **Tests**: `./test.sh` passes - all tests green.

---

## Resolution (2026-04-10)

**Applied fix:** Moved `is_allocated` check to BEFORE `generation()` read in `GcBoxWeakRef::clone()`.

Code change in `crates/rudo-gc/src/ptr.rs`:
- Lines 818-838: Reordered to check `is_allocated` first, then read `generation()`
- Removed the now-redundant `is_allocated` check after `inc_weak()` since slot is guaranteed allocated at that point

**Status: Fixed**
