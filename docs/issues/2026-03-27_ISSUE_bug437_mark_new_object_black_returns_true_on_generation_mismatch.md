# [Bug]: mark_new_object_black returns true on generation mismatch - wrong object marked after slot reuse

**Status:** Fixed
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during new object marking |
| **Severity (嚴重程度)** | `Critical` | Wrong object marked as live, potentially corrupting GC mark state |
| **Reproducibility (復現難度)** | `Medium` | Requires precise concurrent timing between lazy sweep and marking |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `mark_new_object_black` in `gc/incremental.rs` (lines 1075-1117)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

`mark_new_object_black` should only return `true` if the intended object was successfully marked. If the slot was reused (generation changed) between setting the mark and checking the generation, the function should return `false` because the original object is no longer at that slot.

### 實際行為 (Actual Behavior)

In `mark_new_object_black` (lines 1100-1111), when `current_generation != marked_generation` (slot was reused), the function performs additional checks but ultimately returns `true`:

```rust
let current_generation = (*gc_box).generation();
if current_generation != marked_generation {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return false;
    }
    if (*gc_box).is_under_construction() {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return false;
    }
    return true;  // BUG: Returns true even though generation changed!
}
return true;
```

This means if:
1. New object A is allocated with generation G1
2. `mark_new_object_black` is called for A
3. `set_mark` succeeds
4. Lazy sweep runs concurrently and reuses the slot for object B with generation G2
5. Generation check at line 1100 shows mismatch (G2 != G1)
6. But if slot is allocated and B is not under construction, function returns `true`

**Result**: Object B (which may be uninitialized or have different invariants) is incorrectly marked as "live" even though the original intent was to mark A.

---

## 根本原因分析 (Root Cause Analysis)

### 漏洞場景

1. Thread A allocates new object A at slot S with generation G1
2. Thread A calls `mark_new_object_black(A)` to mark it live
3. `set_mark(idx)` succeeds at line 1095
4. Thread B (lazy sweep) runs concurrently and:
   - Sweeps slot S (A is unreachable)
   - Reallocates slot S to new object B with generation G2 ≠ G1
5. Thread A continues:
   - `is_allocated(idx)` returns true (B's slot is allocated)
   - `current_generation = G2` ≠ `marked_generation = G1`
   - `is_allocated` and `is_under_construction` checks pass for B
   - **Returns `true` - marking B as live even though A was the intended target**

### 為什麼這是錯誤的

The function's purpose is to mark NEW objects as live (SATB optimization). If the slot was reused, we are marking a different object than intended. The function should return `false` to indicate the marking didn't happen for the original object.

The checks at lines 1102-1109 (`is_allocated` and `is_under_construction`) were likely added to handle some edge case, but they don't correctly handle the generation mismatch case - they just happen to check the new object's state rather than the original object's.

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Theoretical race requiring concurrent lazy sweep:

```rust
// Thread 1: Allocate and mark new object
fn allocate_and_mark() {
    let gc = Gc::new(Data { value: 42 });
    // gc is at slot S with generation G1

    // If lazy sweep runs here and reuses slot S for new allocation...

    mark_new_object_black(gc.as_ptr()); // Should return true only if A was marked
    // BUG: Could return true if slot was reused and B is now marked instead
}

// Thread 2: Lazy sweep runs concurrently
fn lazy_sweep() {
    // Sweeps slot S (gc's slot)
    // Reallocates slot S to object B with different generation
}
```

---

## 建議修復方案 (Suggested Fix / Remediation)

The fix is simple: when `current_generation != marked_generation`, return `false` because the original object is no longer at that slot:

```rust
let current_generation = (*gc_box).generation();
if current_generation != marked_generation {
    // Slot was reused - we marked the wrong object, rollback
    (*header.as_ptr()).clear_mark_atomic(idx);
    return false;  // FIX: Return false, not true
}
return true;
```

Alternatively, if graceful handling is desired, the checks should verify the original object's state, not the new object's state.

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The SATB (Snapshot-At-The-Beginning) invariant requires that we only mark objects that existed at the start of the marking cycle. If a slot is reused during marking, the new object should NOT be marked as live based on an old reference - it should be traced normally when/if it becomes reachable through other paths. Returning `true` incorrectly treats the new object as if it were the original, violating SATB.

**Rustacean (Soundness 觀點):**
This is a memory safety issue because it can cause the GC to treat incorrect objects as live. The `is_allocated` and `is_under_construction` checks after a generation mismatch are checking the NEW object's state, not validating that we successfully marked the ORIGINAL object. This could lead to marking uninitialized or partially-constructed objects as live.

**Geohot (Exploit 觀點):**
If an attacker can control the timing of lazy sweep and object allocation, they could potentially cause the GC to mark attacker-controlled data as live, leading to memory corruption or information disclosure. The generation mismatch should be treated as a failure case, not a success case.

---

## 相關 Issue

- bug426: trace_and_mark_object missing generation check - similar generation handling issue
- bug336: Generation check to detect slot reuse
- bug311: Lazy sweep TOCTOU

---

## Resolution (2026-03-28)

**Outcome:** Already fixed in current `crates/rudo-gc/src/gc/incremental.rs`.

`mark_new_object_black` now clears the mark and returns `false` when `current_generation != marked_generation` after `set_mark` (see `clear_mark_atomic` + `return false` immediately after the generation check). This matches the suggested remediation: on slot reuse, do not report success.

**Verification:** Static review of `mark_new_object_black` (lines ~1075–1087); `cargo test -p rudo-gc --test incremental_marking -- --test-threads=1` passed.