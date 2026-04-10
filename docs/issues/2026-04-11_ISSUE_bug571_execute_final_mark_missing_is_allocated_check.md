# [Bug]: execute_final_mark Missing is_allocated Check Before Reading Generation

**Status:** Open
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Occurs when lazy sweep deallocates slot between enqueue and pop |
| **Severity (嚴重程度)** | Critical | Undefined behavior - reads generation from deallocated memory |
| **Reproducibility (重現難度)** | Low | Requires specific timing between lazy sweep and incremental marking |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `execute_final_mark` (gc/incremental.rs:988-997)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `execute_final_mark` pops an entry from the worklist, it should verify the slot is still allocated before reading any GcBox fields.

### 實際行為 (Actual Behavior)
`execute_final_mark` reads `generation()` directly without first verifying `is_allocated`, causing UB when the slot has been swept (deallocated but not reused).

---

## 根本原因分析 (Root Cause Analysis)

**問題位置：** `gc/incremental.rs:988-997`

```rust
while let Some((ptr, enqueue_generation)) = visitor.worklist.pop() {
    // FIX bug512: Verify slot wasn't reused since enqueue.
    unsafe {
        let current_generation = (*ptr.as_ptr()).generation();  // BUG!
        if current_generation != enqueue_generation {
            continue; // Slot was reused - skip this entry
        }
        state.push_work(ptr);
    }
    total_marked += 1;
}
```

**對比正確實作：** `execute_snapshot` (lines 594-612) 和 `trace_and_mark_object` (lines 813-829) 都有正確的 `is_allocated` 檢查：

```rust
while let Some((ptr, enqueue_generation)) = visitor.worklist.pop() {
    unsafe {
        // FIX bug565: Check is_allocated BEFORE reading generation.
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                continue; // Slot was swept - skip this entry
            }
        }
        let current_generation = (*ptr.as_ptr()).generation();
        // ...
    }
}
```

**問題場景：**
1. Object A is marked and pushed to `visitor.worklist` with generation G
2. Lazy sweep deallocates the slot (marks as unallocated but does NOT reuse)
3. `execute_final_mark` pops `(ptr, G)` from worklist
4. **BUG**: `generation()` is read from deallocated memory
5. Generation check passes (G == G, because slot is empty not reused)
6. **Result**: UB from reading deallocated memory

---

## 重現步驟 / 概念驗證 (PoC)

```rust
// This bug is triggered by specific timing between:
// 1. Object pushed to worklist during marking
// 2. Lazy sweep deallocates the slot (but doesn't reuse)
// 3. execute_final_mark processes the worklist entry

// The bug is latent - the generation check catches slot REUSE
// but NOT simple deallocation. When a slot is deallocated (not reused),
// its generation appears unchanged, so the check passes, but we're
// reading from deallocated memory.
```

---

## 建議修復方案 (Suggested Fix)

Add `is_allocated` check before reading generation, matching the pattern used in `execute_snapshot` (lines 594-612) and `trace_and_mark_object` (lines 813-829):

```rust
while let Some((ptr, enqueue_generation)) = visitor.worklist.pop() {
    unsafe {
        // FIX bug565: Check is_allocated BEFORE reading generation.
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                continue; // Slot was swept - skip this entry
            }
        }
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != enqueue_generation {
            continue; // Slot was reused - skip this entry
        }
        state.push_work(ptr);
    }
    total_marked += 1;
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check was added to detect slot REUSE (bug512), but it doesn't handle simple deallocation. When a slot is swept and NOT immediately reused, its generation value remains the same, so the generation check passes but we're reading from deallocated memory.

**Rustacean (Soundness 觀點):**
This is a clear undefined behavior bug. Reading from deallocated memory is UB in Rust, regardless of whether the value "looks correct". The fix pattern (is_allocated check before any field access) is consistent across the codebase.

**Geohot (Exploit 觀點):**
If an attacker can influence GC timing to trigger this bug, they could potentially read stale generation values from freed memory. Combined with other vulnerabilities, this could aid in exploitation.