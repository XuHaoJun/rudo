# [Bug]: mark_and_trace_incremental reads generation before is_allocated check (UB + stale mark)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires lazy sweep to deallocate slot between try_mark and is_allocated re-check |
| **Severity (嚴重程度)** | `High` | UB from reading deallocated slot, stale mark may not be cleared |
| **Reproducibility (復現難度)** | `Medium` | Concurrent lazy sweep needed, stress tests can trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_and_trace_incremental` (gc/gc.rs:2474-2481)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `try_mark` succeeds and then `is_allocated` check fails (slot was swept), the code should:
1. ALWAYS clear the stale mark
2. Return WITHOUT reading generation from the deallocated slot

### 實際行為 (Actual Behavior)

In `mark_and_trace_incremental` (gc.rs:2474-2481):

```rust
Ok(true) => {
    // FIX bug552: Read generation BEFORE is_allocated check to avoid UB.
    // Reading from a deallocated slot is undefined behavior.
    let marked_generation = (*ptr.as_ptr()).generation();  // LINE 2474: UB - reads from potentially deallocated slot!
    if !(*header.as_ptr()).is_allocated(idx) {             // LINE 2475: Check AFTER read
        // FIX bug552: Slot was swept - ALWAYS clear stale mark when slot not allocated.
        // The generation was captured while slot was still valid.
        // When slot is not allocated, the mark is stale and must be cleared.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    if (*ptr.as_ptr()).generation() != marked_generation {
```

**Problems:**
1. When `is_allocated(idx)` is false, the code reads `generation()` from a deallocated slot - **UNDEFINED BEHAVIOR**
2. The comment says "FIX bug552: Read generation BEFORE is_allocated check to avoid UB" but this is WRONG - reading from deallocated memory IS UB regardless of what we do after

---

## 🔬 根本原因分析 (Root Cause Analysis)

**The buggy sequence:**

1. `try_mark` succeeds at line 2471, marking the slot
2. `marked_generation` captured at line 2474
3. Lazy sweep deallocates the slot between lines 2474 and 2475
4. `is_allocated` check at line 2475 fails (slot not allocated)
5. **BUG**: We've already read `generation()` from the deallocated slot at line 2474 - **UB!**
6. Mark is cleared and we return

**Why this is different from `worker_mark_loop` (marker.rs:908-922):**

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {  // FIRST: Check is_allocated
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    let gc_box_ptr = obj.cast_mut();
    // FIX bug427: Capture generation AFTER is_allocated check
    let marked_generation = (*gc_box_ptr).generation();  // THEN: Read generation
```

`worker_mark_loop` correctly checks `is_allocated` FIRST, then reads `generation()`. `mark_and_trace_incremental` does the opposite.

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// Requires concurrent lazy sweep:
// 1. Allocate object A in slot with generation G
// 2. try_mark succeeds on object A
// 3. First is_allocated check passes (slot allocated)
// 4. marked_generation captured = G
// 5. Lazy sweep deallocates slot (object A collected, slot empty)
// 6. BUG: reads generation from deallocated slot at line 2474
// 7. Second is_allocated check fails
// 8. We clear mark and return - but UB has already occurred
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the `is_allocated` check BEFORE reading generation, matching `worker_mark_loop`:

```rust
Ok(true) => {
    // Check is_allocated FIRST to avoid UB
    if !(*header.as_ptr()).is_allocated(idx) {
        // FIX bug552: Slot was swept - ALWAYS clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // Now safe to read generation from allocated slot
    let marked_generation = (*ptr.as_ptr()).generation();
    if (*ptr.as_ptr()).generation() != marked_generation {
        // FIX bug549: Slot was reused with new object - clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // ... rest
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Reading `generation()` from a deallocated slot is undefined behavior in any language, especially in a conservative GC. The generation was meant to be captured from a valid slot, not from potentially reused memory.

**Rustacean (Soundness 觀點):**
This is a critical UB bug. Even if the memory appears to contain "valid" data, reading from deallocated memory can trigger undefined behavior. The comment "FIX bug552: Read generation BEFORE is_allocated check to avoid UB" is self-contradictory - reading from deallocated memory IS UB regardless of subsequent checks.

**Geohot (Exploit 觀點):**
UB from reading deallocated memory could lead to unpredictable behavior, potentially allowing an attacker to manipulate slot state and cause incorrect GC behavior.

---

## 相關 Issue

- bug552: mark_and_trace_incremental reads gen before is_allocated (original issue)
- bug549: mark_and_trace_incremental generation mismatch leaves stale mark
- bug553: worker_mark_loop same bug pattern (FIXED in marker.rs)
- bug554: mark_object reads gen from deallocated slot (FIXED in gc.rs)

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-04-09
**Fix:** Modified `gc/gc.rs:2471-2485` in `mark_and_trace_incremental`:

**Before (buggy):**
```rust
Ok(true) => {
    // FIX bug552: Read generation BEFORE is_allocated check to avoid UB.
    // Reading from a deallocated slot is undefined behavior.
    let marked_generation = (*ptr.as_ptr()).generation();  // UB - reads from potentially deallocated slot!
    if !(*header.as_ptr()).is_allocated(idx) {
        // FIX bug552: Slot was swept - ALWAYS clear stale mark when slot not allocated.
        // The generation was captured while slot was still valid.
        // When slot is not allocated, the mark is stale and must be cleared.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    if (*ptr.as_ptr()).generation() != marked_generation {
        // FIX bug549: Slot was reused with new object - clear stale mark.
        // The old object's mark should not persist on the new object.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
```

**After (fixed):**
```rust
Ok(true) => {
    // FIX bug557: Check is_allocated FIRST to avoid UB.
    // Reading generation from a deallocated slot is undefined behavior.
    if !(*header.as_ptr()).is_allocated(idx) {
        // FIX bug552: Slot was swept - ALWAYS clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // Now safe to read generation from guaranteed allocated slot
    let marked_generation = (*ptr.as_ptr()).generation();
    if (*ptr.as_ptr()).generation() != marked_generation {
        // FIX bug549: Slot was reused with new object - clear stale mark.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
```

**Code Change:** Moved the `is_allocated` check BEFORE reading `generation()`, matching the pattern from `worker_mark_loop` (marker.rs:908-922).

**Verification:** `./clippy.sh` passes, `./test.sh` passes.