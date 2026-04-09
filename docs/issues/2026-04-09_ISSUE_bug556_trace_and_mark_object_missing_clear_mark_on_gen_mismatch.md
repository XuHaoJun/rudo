# [Bug]: trace_and_mark_object missing clear_mark_atomic on generation mismatch (bug556)

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during incremental marking with slot reuse |
| **Severity (嚴重程度)** | `High` | Stale mark causes incorrect GC behavior; objects may be incorrectly retained |
| **Reproducibility (復現難度)** | `Medium` | Requires precise concurrent timing between mark and slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `trace_and_mark_object` in `gc/incremental.rs` (lines 785-789)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

After `try_mark` succeeds and a generation mismatch is detected (slot was swept and reused), the code should call `clear_mark_atomic(idx)` to clear the stale mark before returning. This prevents leaving a mark on a slot that now contains a different object.

### 實際行為 (Actual Behavior)

In `trace_and_mark_object` (incremental.rs:785-789), when generation mismatch is detected, the function returns WITHOUT clearing the mark:

```rust
// Verify generation hasn't changed before calling trace_fn (bug426 fix).
// If slot was reused, trace_fn would be called on wrong object data.
if (*gc_box.as_ptr()).generation() != marked_generation {
    return;  // BUG: No clear_mark_atomic(idx) here!
}
```

**Comparison with `mark_object_minor`** (gc.rs:2107-2110):
```rust
if (*ptr.as_ptr()).generation() != marked_generation {
    (*header.as_ptr()).clear_mark_atomic(index);
    return;
}
```

**Comparison with `mark_and_trace_incremental`** (gc.rs:2482-2486):
```rust
if (*ptr.as_ptr()).generation() != marked_generation {
    // FIX bug549: Slot was reused with new object - clear stale mark.
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Scenario triggering the bug:**

1. Object A allocated in slot with generation 5
2. During incremental mark, `trace_and_mark_object` is called for Object A
3. `marked_generation` captured = 5 at line 779
4. Between lines 779 and 787: lazy sweep deallocates slot, Object B allocated with generation 6
5. Generation check at line 787: 6 != 5, returns WITHOUT clearing mark
6. Slot now has Object B with a stale mark from Object A
7. Object B may be incorrectly considered marked/alive in future GC cycles

**The bug pattern:**

The function checks generation at line 787, detects mismatch, but does NOT clear the mark before returning. This leaves a stale mark on the reused slot.

**Why the pattern is inconsistent:**

- `mark_object_minor` (bug546 fix): Clears mark on generation mismatch ✓
- `mark_and_trace_incremental` (bug549 fix): Clears mark on generation mismatch ✓
- `trace_and_mark_object`: Returns without clearing mark ✗

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical bug - requires specific concurrent interleaving
// 1. Object A allocated in slot with generation N
// 2. trace_and_mark_object called for Object A
// 3. marked_generation captured = N
// 4. Lazy sweep deallocates slot, Object B allocated with generation N+1
// 5. Generation check fails, returns without clearing mark
// 6. Object B incorrectly retains mark from Object A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `clear_mark_atomic(idx)` before the return when generation mismatch is detected:

```rust
// FIX bug556: Clear stale mark when generation mismatch.
// When slot is reused, the old object's mark should not persist on the new object.
if (*gc_box.as_ptr()).generation() != marked_generation {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The mark bit should always be associated with the object that set it. When a slot is reused, the new object "inherits" the mark only if it was set AFTER the reuse, not before. The current code leaves a stale mark from the old object on the new object's slot.

**Rustacean (Soundness 觀點):**
This is not immediate UB, but can lead to incorrect GC behavior. Objects may be incorrectly retained because they appear "marked" even though they should have been swept. The inconsistency with `mark_object_minor` and `mark_and_trace_incremental` suggests the bug426 fix was incomplete.

**Geohot (Exploit 觀點):**
In a concurrent scenario, an attacker could potentially manipulate allocation patterns to cause stale marks to persist, leading to memory exhaustion (objects not being collected). While difficult to exploit directly, the memory leak aspect could be leveraged in a denial-of-service context.

---

## 相關 Issue

- bug426: trace_and_mark_object missing generation check before trace_fn (original fix)
- bug549: mark_and_trace_incremental missing clear_mark on gen mismatch (similar bug)
- bug546: mark_object_minor missing clear_mark on gen mismatch (similar bug, FIXED)
- bug555: mark_object_black reads generation from deallocated slot (similar pattern)