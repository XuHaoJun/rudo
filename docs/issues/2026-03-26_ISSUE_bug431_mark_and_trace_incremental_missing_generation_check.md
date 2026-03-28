# [Bug]: mark_and_trace_incremental missing generation check before trace_fn after slot reuse TOCTOU

**Status:** Fixed
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during incremental marking |
| **Severity (嚴重程度)** | `High` | Calls trace_fn on wrong object data after slot reuse |
| **Reproducibility (重現難度)** | `Medium` | Requires precise concurrent timing between try_mark and trace_fn |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `mark_and_trace_incremental` in `gc/gc.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.x`

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

After `try_mark` succeeds, the code should verify the generation hasn't changed before calling `trace_fn`. If slot was swept and reused (generation changed), `trace_fn` should NOT be called to avoid calling it on wrong object data.

### 實際行為 (Actual Behavior)

In `mark_and_trace_incremental` (gc.rs:2464-2478), after `try_mark` succeeds and `is_allocated` is verified TRUE at line 2469, the code proceeds directly to `visitor.objects_marked += 1` and eventually calls `trace_fn` WITHOUT checking if the generation changed.

**Bug Pattern:**

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {  // Line 2465
        return;
    }
    let marked_generation = (*ptr.as_ptr()).generation();  // Line 2468
    if !(*header.as_ptr()).is_allocated(idx) {  // Line 2469
        // Generation check only happens HERE (when is_allocated is false)
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != marked_generation {
            return;
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;  // Line 2477
    break;                        // Line 2478
    // BUG: No generation check when is_allocated is TRUE!
}
```

Compare with `worker_mark_loop` (bug427 fix) which correctly checks generation BEFORE calling `trace_fn`:

```rust
let marked_generation = (*gc_box_ptr).generation();
if !(*header.as_ptr()).is_allocated(idx) {
    // ...
}
if (*gc_box_ptr).generation() != marked_generation {
    break; // Slot was reused - skip
}
marked += 1;
(((*gc_box_ptr).trace_fn)(ptr_addr, &mut visitor);  // trace_fn called AFTER generation check
```

---

## 根本原因分析 (Root Cause Analysis)

The bug is a missing generation check after line 2476 (when `is_allocated` is TRUE at line 2469). The generation check pattern follows bug426 and bug427 fixes, but was not applied consistently.

**Scenario triggering the bug:**

1. `try_mark` succeeds on Object A at generation 5
2. `is_allocated` check at line 2465 passes (slot is allocated with Object A)
3. `marked_generation` captured = 5 (line 2468)
4. Between line 2468 and 2469: slot is swept and Object B is allocated (generation = 6)
5. `is_allocated` check at line 2469 passes (slot is allocated with Object B)
6. `visitor.objects_marked += 1` (line 2477) - Object B's mark incremented!
7. `trace_fn` called on Object B's data - **WRONG OBJECT**

The generational barrier at entry (lines 2444-2448) provides some protection, but does not cover all cases where slot reuse occurs after `try_mark`.

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical bug - requires specific concurrent interleaving
// 1. Object A allocated in slot with generation 5
// 2. During incremental mark, try_mark succeeds on Object A
// 3. marked_generation captured = 5
// 4. Lazy sweep deallocates slot and allocates Object B (generation 6)
// 5. is_allocated returns true (slot now has Object B)
// 6. trace_fn called on Object B's data (wrong object!)
// 7. Result: Object B's trace_fn called with wrong data pointer
```

---

## 建議修復方案 (Suggested Fix / Remediation)

Add generation check after line 2476 and before line 2477, matching the pattern from `worker_mark_loop`:

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        return;
    }
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != marked_generation {
            return;
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // FIX bug431: Verify generation hasn't changed before calling trace_fn.
    // If slot was reused between try_mark and here, skip to avoid calling
    // trace_fn on wrong object data.
    if (*ptr.as_ptr()).generation() != marked_generation {
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is critical in concurrent GC environments. Without it, `trace_fn` can be called on an object that was allocated after `try_mark` succeeded, leading to incorrect tracing of the wrong object's data. This is a classic TOCTOU bug that was partially fixed in bug426 and bug427 but missed in `mark_and_trace_incremental`.

**Rustacean (Soundness 觀點):**
While not immediate UB (memory is valid), calling `trace_fn` on wrong object data can corrupt the GC's view of the heap, potentially leading to premature collection of live objects or retention of dead ones. The inconsistency with `worker_mark_loop` which has the check suggests this was an oversight during the bug426/bug427 fixes.

**Geohot (Exploit 觀點):**
In a concurrent scenario, an attacker could influence allocation patterns to cause `trace_fn` to be called on a crafted object, potentially leading to memory corruption or information disclosure if the object layout overlaps with sensitive data.

---

## 相關 Issue

- bug426: trace_and_mark_object missing generation check - similar issue in incremental.rs
- bug427: worker_mark_loop missing generation check - correct pattern that should be followed
- bug398: mark_and_trace_incremental TOCTOU - original issue that was partially fixed
- bug295: TOCTOU between is_allocated check and set_mark - root cause pattern
- bug336: incremental marking TOCTOU with lazy sweep reallocation - similar pattern

---

**Resolution (2026-03-28):** Verified in `gc.rs`: after `Ok(true)` from `try_mark`, when both `is_allocated` checks pass, the code compares `(*ptr.as_ptr()).generation()` to `marked_generation` and returns early on mismatch (lines ~2469–2471), matching the `worker_mark_loop` / bug427 pattern. No further code change required. `./test.sh` passed.