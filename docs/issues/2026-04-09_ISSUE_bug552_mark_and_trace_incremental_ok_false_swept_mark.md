# [Bug]: mark_and_trace_incremental returns without clearing mark when try_mark returns Ok(false) and slot swept

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep after another thread marks |
| **Severity (嚴重程度)** | High | Stale mark left on swept slot, causing incorrect object retention |
| **Reproducibility (復現難度)** | Medium | Concurrent lazy sweep needed |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_and_trace_incremental` (gc/gc.rs:2460-2464)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When `try_mark` returns `Ok(false)` (slot was already marked by another thread) AND the slot is no longer allocated (swept), the code should clear any stale mark before returning. The mark is stale and belongs to the dead object.

### 實際行為 (Actual Behavior)

In `mark_and_trace_incremental` (gc.rs:2460-2464):

```rust
Ok(false) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        return;  // BUG: Returns WITHOUT clearing mark when slot swept!
    }
    return; // Already marked by another thread, no push needed
}
```

When `is_allocated(idx)` is false, the code returns WITHOUT clearing the mark. This is wrong because:
1. The slot was swept (object is gone) - any mark is stale
2. Even if another thread marked it first, if the slot is now swept, the mark should be cleared
3. A stale mark could cause the GC to retain invalid references

---

## 🔬 根本原因分析 (Root Cause Analysis)

**The buggy sequence:**

1. Thread A: `try_mark` succeeds at line 2459, marking the slot
2. Thread B: `try_mark` on same slot returns `Ok(false)` (already marked)
3. Between line 2459 and 2461: lazy sweep deallocates the slot
4. Thread B: `is_allocated(idx)` check at line 2461 fails (slot not allocated)
5. Thread B: Returns at line 2462 WITHOUT clearing the mark

**Why this is wrong:**

When `is_allocated` is false, the object is gone and the mark is stale. We should ALWAYS clear the mark in this case, regardless of who set it originally. The `Ok(false)` path should handle this just like the `Ok(true)` path.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires concurrent lazy sweep:
// 1. Allocate object A in slot
// 2. Thread A: try_mark succeeds on object A (marks slot)
// 3. Thread B: try_mark returns Ok(false) (slot already marked)
// 4. Lazy sweep deallocates slot (object A collected)
// 5. Thread B: is_allocated check fails (slot not allocated)
// 6. Thread B returns WITHOUT clearing mark - stale mark persists
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add mark clearing when slot is not allocated in the `Ok(false)` path:

```rust
Ok(false) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        // FIX BUGXXX: Slot was swept - clear stale mark before returning.
        // Even though another thread marked it, the mark is stale now.
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    return; // Already marked by another thread, no push needed
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The mark bit should always be associated with a live object. When a slot is swept, any mark left behind is stale and must be cleared. The `Ok(false)` path should mirror the `Ok(true)` path's handling of swept slots.

**Rustacean (Soundness 觀點):**
A stale mark could cause incorrect GC behavior. Objects may be incorrectly retained because they appear "marked" even though the slot is no longer allocated. This violates the GC's correctness guarantees.

**Geohot (Exploit 觀點):**
A stale mark could be leveraged in a denial-of-service attack if an attacker can trigger lazy sweep at specific times to leave marks on reclaimed slots, causing memory exhaustion.

---

## 相關 Issue

- bug550: mark_and_trace_incremental same bug (returns without clearing mark when slot swept)
- bug549: mark_and_trace_incremental generation mismatch should clear stale mark
- bug547: mark_and_trace_incremental missing is_under_construction check