# [Bug]: mark_object_black reads generation from deallocated slot (bug555)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires lazy sweep to deallocate slot between is_under_construction check and try_mark |
| **Severity (嚴重程度)** | `High` | UB from reading deallocated slot |
| **Reproducibility (復現難度)** | `Medium` | Concurrent lazy sweep needed, stress tests can trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_black` (gc/incremental.rs:1183-1206)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When `try_mark` succeeds and then `is_allocated` check at line 1187 fails (slot was swept), the code should:
1. Clear the stale mark without reading generation from the deallocated slot
2. Return None

### 實際行為 (Actual Behavior)

In `mark_object_black` (incremental.rs:1183-1206):

```rust
Ok(true) => {
    let marked_generation = (*gc_box).generation();
    if (*h).is_allocated(idx) {
        // ...
        return Some(idx);
    }
    // BUG: Slot not allocated - reading from potentially deallocated slot!
    let current_generation = (*gc_box).generation();  // Line 1198 - UB!
    if current_generation != marked_generation {
        return None;
    }
    (*h).clear_mark_atomic(idx);  // Line 1204 - clear mark
    return None;
}
```

**Problem:**
When `is_allocated(idx)` is false at line 1187, the code reads `current_generation` at line 1198 from a potentially deallocated slot - undefined behavior!

---

## 🔬 根本原因分析 (Root Cause Analysis)

**The buggy sequence:**

1. Line 1159: `gc_box = &*ptr.cast::<GcBox<()>>()` obtains reference (valid at this point)
2. Line 1164: Second `is_allocated` check passes (slot still allocated)
3. Line 1168: `gc_box.is_under_construction()` called
4. Line 1173: `try_mark` succeeds - we marked the slot
5. Line 1185: `marked_generation` captured from `gc_box`
6. Line 1187: `is_allocated(idx)` returns FALSE - slot was swept between checks!
7. **BUG**: Line 1198 reads `current_generation` from deallocated slot (UB!)
8. If generations match, we clear mark and return (correct path when slot was swept but not reused)

**The fix should be:**
When `is_allocated(idx)` is false, immediately clear the mark without reading generation. The generation check is only useful for distinguishing swept+reused from swept-only, but we can skip it because if slot is not allocated, it was definitely swept (not reused).

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// Requires concurrent lazy sweep:
// 1. Allocate object A in slot
// 2. gc_box obtained at line 1159 (valid reference)
// 3. First is_allocated check at line 1164 passes (slot allocated)
// 4. is_under_construction check passes
// 5. try_mark succeeds on object A
// 6. marked_generation captured at line 1185
// 7. Lazy sweep deallocates the slot (object A collected)
// 8. is_allocated check at line 1187 returns false
// 9. BUG: reads current_generation from deallocated slot (line 1198)
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

When slot is not allocated at line 1187, immediately clear mark and return None without reading generation:

```rust
Ok(true) => {
    let marked_generation = (*gc_box).generation();
    if (*h).is_allocated(idx) {
        let current_generation = (*gc_box).generation();
        if current_generation != marked_generation {
            return None;
        }
        return Some(idx);
    }
    // FIX bug555: Slot not allocated - clear mark immediately without reading generation.
    // Reading generation from deallocated slot is UB.
    (*h).clear_mark_atomic(idx);
    return None;
}
```

Or better, use the same pattern as mark_and_trace_incremental:
```rust
if !(*h).is_allocated(idx) {
    (*h).clear_mark_atomic(idx);
    return None;
}
if (*gc_box).generation() != marked_generation {
    (*h).clear_mark_atomic(idx);
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check serves to distinguish swept-only from swept-and-reused slots. When `is_allocated` is false, the slot was definitely swept (not reused by a new allocation). We can skip the generation check entirely and just clear the mark.

**Rustacean (Soundness 觀點):**
Reading from a deallocated slot is undefined behavior. The fix is to clear the mark and return when `is_allocated` is false, without reading generation.

**Geohot (Exploit 觀點):**
UB from reading deallocated memory combined with potential stale marks could lead to correctness issues. The fix is straightforward - don't read from unallocated slots.

---

## 相關 Issue

- bug554: mark_object same bug pattern (fixed in gc.rs)
- bug553: worker_mark_loop had similar issue (fixed in marker.rs)
- bug549: mark_and_trace_incremental generation mismatch should clear stale mark
- bug547: mark_and_trace_incremental missing is_under_construction check