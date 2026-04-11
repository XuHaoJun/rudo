# [Bug]: scan_page_for_unmarked_refs CAS retry in Err branch without is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires CAS failure to occur simultaneously with slot deallocation by lazy sweep |
| **Severity (嚴重程度)** | High | UB - reading generation from deallocated slot during retry |
| **Reproducibility (重現難度)** | Very Low | Requires Miri or ThreadSanitizer to detect |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_unmarked_refs` (gc/incremental.rs:1108)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-04-11
**Fix:** Added `is_allocated` check in `Err` branch of `scan_page_for_unmarked_refs` (incremental.rs:1108-1115). The check prevents retrying the CAS on a deallocated slot, which would cause UB. Matches the pattern in `scan_page_for_marked_refs` (bug576 fix).

**Code Change:**
```rust
// Before (BUG):
Err(()) => {
    // CAS failed - another thread modified this word.
    // Retry the CAS to get a consistent view.
}

// After (FIX):
Err(()) => {
    // FIX bug581: Check is_allocated before retry to avoid UB from deallocated slot.
    // If CAS failed because lazy sweep deallocated the slot, retrying on a
    // deallocated slot is UB. Matches scan_page_for_marked_refs (bug576) pattern.
    if !(*header).is_allocated(i) {
        break;
    }
}
```

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When CAS fails in `try_mark` and the slot has been deallocated by lazy sweep, the loop should check `is_allocated` before retrying, similar to the fix in bug576 for `scan_page_for_marked_refs`.

### 實際行為 (Actual Behavior)

The `Err(())` branch in `scan_page_for_unmarked_refs` (incremental.rs:1108) does nothing and retries the loop without checking if the slot is still allocated:

```rust
Err(()) => {
    // CAS failed - another thread modified this word.
    // Retry the CAS to get a consistent view.
} // BUG: No is_allocated check!
```

If lazy sweep deallocates the slot between CAS failures, the retry reads from a deallocated slot, causing undefined behavior.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `gc/incremental.rs:1108`

```rust
loop {
    match (*header).try_mark(i) {
        Ok(false) => {
            // Already marked by another thread; move to next slot.
            break;
        }
        Ok(true) => {
            // Has is_allocated checks before reading generation
            // ...
        }
        Err(()) => {
            // BUG: No is_allocated check!
        }
    }
}
```

**對比 `scan_page_for_marked_refs` (bug576 fix at incremental.rs:910):**

`scan_page_for_marked_refs` was already fixed (bug576) to include the check:

```rust
Err(()) => {
    if !(*header).is_allocated(i) {
        break;
    }
} // CAS failed, retry
```

But `scan_page_for_unmarked_refs` was not updated with the same fix.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Requires ThreadSanitizer or Miri to detect the data race between CAS failure and slot deallocation.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check in `Err` branch, matching the pattern in `scan_page_for_marked_refs` (bug576 fix):

```rust
Err(()) => {
    // Check is_allocated before retry to avoid UB from deallocated slot.
    if !(*header).is_allocated(i) {
        break;
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The CAS failure could occur after lazy sweep deallocates the slot. The retry should verify the slot is still allocated before continuing.

**Rustacean (Soundness 觀點):**
Reading from a deallocated slot is undefined behavior. The `is_allocated` check must be performed before any retry.

**Geohot (Exploit 觀點):**
While the race window is small, an attacker who could influence GC timing might trigger UB by causing the CAS to fail exactly when lazy sweep deallocates the slot.

---

## 相關 Issue

- bug576: Same bug pattern in scan_page_for_marked_refs (fixed)
- bug575: Same bug pattern in worker_mark_loop (fixed)
- bug574: Same bug pattern in mark_object_minor, mark_object (fixed)
- bug573: Same bug pattern in mark_and_push_to_worker_queue (fixed)
