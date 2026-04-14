# [Bug]: mark_object_black Err branch retries CAS without is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | CAS failure with sweep during retry window is rare but possible |
| **Severity (嚴重程度)** | High | UB from deallocated slot access during CAS retry loop |
| **Reproducibility (復現難度)** | Medium | Requires concurrent lazy sweep during incremental mark |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_black` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

In `mark_object_black` (incremental.rs:1234-1260), the `Err(())` branch (line 1257) simply retries the loop without checking if the slot is still allocated:

```rust
Err(()) => {} // CAS failed, retry
```

If lazy sweep deallocates the slot between the initial `is_allocated` check and the CAS failure, the loop retries on a deallocated slot. This is undefined behavior - reading from deallocated memory.

### 預期行為 (Expected Behavior)
After CAS failure (`Err(())`), the code should check `is_allocated` before retrying. If the slot was swept, return `None` to avoid UB.

### 實際行為 (Actual Behavior)
The `Err(())` branch retries without checking if the slot is still allocated. If the slot was deallocated, this causes UB from reading `generation()` or `is_under_construction()` from a deallocated slot.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Location:** `crates/rudo-gc/src/gc/incremental.rs:1257`

**Pattern:** The `Err(())` branch does:
```rust
Err(()) => {} // CAS failed, retry
```

**Bug Pattern:** Other functions in the codebase correctly check `is_allocated` in the `Err` branch:
- `mark_object_minor` (gc.rs:2150-2154) - FIX bug573
- `worker_mark_loop` (marker.rs:943-946) - FIX bug575  
- `scan_page_for_unmarked_refs` (similar pattern) - FIX bug576
- `mark_object_minor` (gc.rs:1237-1244) - FIX bug573

The `mark_object_black` function is missing this check.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudo-PoC: Requires precise timing between:
// 1. mark_object_black initial is_allocated check passes
// 2. CAS fails (Err(()))
// 3. Slot is swept by lazy sweep between CAS and retry
// 4. Retry reads generation from deallocated slot

// This is a TOCTOU race condition that is difficult to reproduce
// in single-threaded tests. Requires ThreadSanitizer or concurrent PoC.
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check in the `Err` branch of `mark_object_black` at line 1257:

```rust
Err(()) => {
    // FIX bug622: Check is_allocated before retry on CAS failure.
    // If lazy sweep deallocated the slot during CAS failure,
    // retrying on deallocated memory is UB.
    if !(*h).is_allocated(idx) {
        return None;
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The `mark_object_black` function is part of the incremental marking path. CAS failures in the marking loop are expected under contention, but the retry must validate slot liveness. This follows the same pattern as `mark_object_minor` and `worker_mark_loop` which were already fixed for this issue.

**Rustacean (Soundness 觀點):**
Reading `generation()` or `is_under_construction()` from a deallocated slot is undefined behavior in Rust. The fix is straightforward - add the same `is_allocated` check pattern used elsewhere.

**Geohot (Exploit 觀點):**
If an attacker can influence GC scheduling to create the exact conditions (CAS failure + sweep during retry window), they could potentially read stale data from freed memory. This is a minor concern but worth fixing for defense-in-depth.