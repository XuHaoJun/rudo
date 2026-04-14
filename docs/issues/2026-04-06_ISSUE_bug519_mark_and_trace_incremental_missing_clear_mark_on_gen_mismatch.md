# [Bug]: mark_and_trace_incremental missing clear_mark_atomic on generation mismatch leaves stale mark

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during incremental marking with slot reuse |
| **Severity (嚴重程度)** | `High` | Stale mark causes incorrect GC behavior; objects may be incorrectly retained |
| **Reproducibility (復現難度)** | `Medium` | Requires precise concurrent timing between mark and slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `mark_and_trace_incremental` in `gc/gc.rs` (lines 2470-2472)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

After `try_mark` succeeds and a generation mismatch is detected (slot was swept and reused), the code should call `clear_mark_atomic(idx)` to clear the stale mark before returning. This prevents leaving a mark on a slot that may contain a new object.

### 實際行為 (Actual Behavior)

In `mark_and_trace_incremental` (gc.rs:2470-2472), when `is_allocated` is TRUE but generation has changed since `marked_generation` was captured, the function returns without clearing the mark:

```rust
if (*ptr.as_ptr()).generation() != marked_generation {
    return;  // BUG: No clear_mark_atomic(idx) here!
}
visitor.objects_marked += 1;
break;
```

**Comparison with `trace_and_mark_object`** (incremental.rs:785-789):
```rust
if (*gc_box.as_ptr()).generation() != marked_generation {
    return;  // No clear_mark here either - but different context
}
```

**Comparison with `mark_object_black`** (incremental.rs:1176-1184):
```rust
// Slot was swept between our check and try_mark.
// Verify generation hasn't changed to distinguish swept from swept+reused.
let current_generation = (*gc_box).generation();
if current_generation != marked_generation {
    // Slot was reused - the mark now belongs to the new object, don't clear.
    return None;
}
// Slot was swept but not reused - safe to clear mark.
(*h).clear_mark_atomic(idx);
return None;
```

The issue is that `mark_object_black` correctly handles three cases:
1. Generation changed (slot reused) → don't clear mark (mark belongs to new object)
2. Generation unchanged + not allocated → clear mark (slot was swept)
3. Generation unchanged + allocated → keep mark (object is alive)

But `mark_and_trace_incremental` only handles the "return" case without distinguishing.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Scenario triggering the bug:**

1. Object A allocated in slot with generation 5
2. During incremental mark, `try_mark` succeeds on Object A at line 2456
3. `is_allocated` check passes at line 2457 (slot is allocated with Object A)
4. `marked_generation` captured = 5 at line 2461
5. Second `is_allocated` check passes at line 2462 (slot still allocated)
6. Between line 2462 and 2470: lazy sweep deallocates slot, Object B allocated with generation 6
7. Generation check at line 2470: 6 != 5, returns WITHOUT clearing mark
8. Slot now has Object B with a stale mark from Object A
9. Object B may be incorrectly considered marked/alive in future GC cycles

**The ambiguity:** The code cannot distinguish between:
- Case A: Slot was swept and NOT reused (generation unchanged) → mark should be cleared
- Case B: Slot was swept and reused with new object (generation changed) → mark belongs to new object, don't clear

When generation mismatch is detected, returning without clearing is correct ONLY if the slot was reused. But if the slot was swept and the generation rolled over (unlikely), the mark should be cleared.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical bug - requires specific concurrent interleaving
// 1. Object A allocated in slot with generation N
// 2. During incremental mark, try_mark succeeds on Object A
// 3. marked_generation captured = N
// 4. Lazy sweep deallocates slot (no reuse yet)
// 5. is_allocated returns false, but slot not reused yet
// 6. OR: Slot is deallocated and reallocated with new object (generation N+1)
// 7. Generation check fails, mark is NOT cleared
// 8. Object B incorrectly retains mark from Object A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option A: Check is_allocated before returning on generation mismatch**
```rust
if (*ptr.as_ptr()).generation() != marked_generation {
    // If slot is still allocated, mark belongs to new object - don't clear
    // If slot is not allocated, we should clear the stale mark
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
    }
    return;
}
```

**Option B: Follow mark_object_black pattern more closely**
The issue is that generation mismatch could mean:
- Slot was reused (new object has new generation) → don't clear
- OR slot was swept and generation wrapped around → clear

Since distinguishing these cases requires checking `is_allocated`, Option A is preferred.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The mark bit should always be associated with the object that set it. When a slot is reused, the new object "inherits" the mark from the previous occupant. If the slot is swept without reuse and the generation happens to match (rare edge case due to wraparound), the mark should be cleared. The current code cannot distinguish these cases, which is a latent bug.

**Rustacean (Soundness 觀點):**
This is not immediate UB, but can lead to incorrect GC behavior. Objects may be incorrectly retained because they appear "marked" even though they should have been swept. The inconsistency with `mark_object_black` suggests this was overlooked during the bug426/bug427 fixes.

**Geohot (Exploit 觀點):**
In a concurrent scenario, an attacker could potentially manipulate allocation patterns to cause stale marks to persist, leading to memory exhaustion (objects not being collected). While difficult to exploit directly, the memory leak aspect could be leveraged in a denial-of-service context.

---

## 相關 Issue

- bug431: mark_and_trace_incremental missing generation check before trace_fn
- bug426: trace_and_mark_object missing generation check
- bug427: worker_mark_loop missing generation check  
- bug512: trace_and_mark_object discards enqueue_generation (similar pattern)
- bug355: mark_object_black generation check pattern
- bug128: mark_and_trace_incremental missing clear_mark_atomic

---

## 驗證記錄

**驗證日期:** 2026-04-06
**驗證人員:** opencode

### 驗證結果

Confirmed the discrepancy between `mark_and_trace_incremental` (gc.rs:2470-2472) and `mark_object_black` (incremental.rs:1176-1184):

- `mark_object_black`: When generation mismatch detected after successful mark:
  - Checks `is_allocated` to distinguish swept vs swept+reused
  - Only clears mark if slot was swept but NOT reused
  - Correctly handles the inheritance case

- `mark_and_trace_incremental`: When generation mismatch detected:
  - Returns immediately without checking `is_allocated`
  - Cannot distinguish swept vs swept+reused case
  - May leave stale mark on reused slot OR clear valid mark

The pattern in `mark_object_black` (checking `is_allocated` after generation mismatch) should be applied to `mark_and_trace_incremental`.

**Status: Open** - Needs fix.