# [Bug]: mark_and_trace_incremental TOCTOU - generation read after loop without is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires lazy sweep to deallocate slot between break and generation read |
| **Severity (嚴重程度)** | High | UB - reading generation from deallocated slot |
| **Reproducibility (復現難度)** | Medium | Race condition window is narrow but real |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_and_trace_incremental` (gc/gc.rs:2499-2510)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
After `try_mark` loop succeeds and breaks, before reading `generation()` for enqueue, we should verify the slot is still allocated.

### 實際行為 (Actual Behavior)

In `mark_and_trace_incremental` (gc.rs:2499-2510):

```rust
Ok(true) => {
    // FIX bug557: Check is_allocated FIRST to avoid UB.
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    let marked_generation = (*ptr.as_ptr()).generation();
    if (*ptr.as_ptr()).generation() != marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    if (*ptr.as_ptr()).is_under_construction() {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;  // LINE 2499: Loop exits successfully
}
// LINE 2508: No is_allocated check before this read!
let enqueue_generation = (*ptr.as_ptr()).generation();  // UB if slot was deallocated!
if visitor.kind != VisitorKind::Minor || enqueue_generation == 0 {
    visitor.worklist.push((ptr, enqueue_generation));
}
```

**Problem**: Between line 2499 (`break`) and line 2508 (read `generation()`), another thread running lazy sweep could deallocate the slot. The `is_allocated` check inside the loop is not re-verified after the break.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Timeline of race:**

1. Thread A: `try_mark` succeeds, `is_allocated(idx)` passes at line 2480
2. Thread A: `marked_generation` captured at line 2486
3. Thread A: Generation check passes at line 2487
4. Thread A: `is_under_construction()` check passes at line 2494
5. Thread A: `break` exits loop at line 2499
6. **Race window**: Lazy sweep deallocates slot (slot empty, not reused)
7. Thread A: Line 2508 reads `generation()` from deallocated slot - **UB!**

**Why generation check doesn't save us:**
- The generation check at line 2487 catches **slot REUSE** (new object has different generation)
- If slot is simply **deallocated** (empty, not reused), generation remains unchanged
- Generation check passes, but we still read from deallocated memory - UB

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudocode - requires precise timing:
// Thread A: mark_and_trace_incremental processes object
// Thread B: lazy sweep deallocates the slot between break and line 2508

// Window: between line 2499 (break) and line 2508 (read generation)
// If sweep deallocates slot in this window, we read from deallocated memory
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check after the loop break, before reading generation for enqueue:

```rust
break;
}
// FIX bug566: Re-verify is_allocated after loop break, before reading generation.
// The slot could have been deallocated by lazy sweep between break and read.
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}

let enqueue_generation = (*ptr.as_ptr()).generation();
if visitor.kind != VisitorKind::Minor || enqueue_generation == 0 {
    visitor.worklist.push((ptr, enqueue_generation));
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic TOCTOU bug in concurrent GC. The is_allocated check inside the loop was correctly placed to avoid UB during the mark decision, but after the loop breaks successfully, we need to re-verify before reading any fields again. Lazy sweep running concurrently creates this narrow but real race window.

**Rustacean (Soundness 觀點):**
Reading from deallocated memory is undefined behavior regardless of what checks were performed earlier. The generation check doesn't protect against simple deallocation - only against reuse. This is a soundness issue that needs fixing.

**Geohot (Exploit 觀點):**
While the race window is narrow, concurrent lazy sweep could trigger this reliably under stress. An attacker could potentially manipulate GC timing to trigger UB. The lack of a second is_allocated check after the break is a latent bug waiting to be exploited under the right conditions.

---

## 相關 Issue

- bug557: mark_and_trace_incremental reads gen before is_allocated (inside loop, FIXED)
- bug565: execute_snapshot worklist pop reads gen before is_allocated (similar pattern)
- bug549: mark_and_trace_incremental generation mismatch leaves stale mark
