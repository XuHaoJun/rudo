# [Bug]: mark_and_push_to_worker_queue missing is_under_construction and second is_allocated re-check before push_work

**Status:** Fixed
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during parallel marking |
| **Severity (嚴重程度)** | `High` | Could push dangling pointer to worker queue, causing incorrect tracing |
| **Reproducibility (復現難度)** | `Very High` | Requires precise concurrent timing between marking and sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Parallel Marking`, `mark_and_push_to_worker_queue` in `gc/gc.rs:1205-1245`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

After successful mark and generation check in `mark_and_push_to_worker_queue`, the code should:
1. Check `is_under_construction` to avoid tracing partially initialized objects (e.g., `Gc::new_cyclic_weak`)
2. Re-check `is_allocated` before `push_work` to fix TOCTOU with lazy sweep

### 實際行為 (Actual Behavior)

The current code at `gc.rs:1225-1243`:

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    let marked_generation = (*gc_box.as_ptr()).generation();
    if (*gc_box.as_ptr()).generation() != marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    break;  // Missing is_under_construction and second is_allocated check!
}
// Line 1243: push_work called without safety re-checks
worker_queues[worker_idx].push(gc_box.as_ptr());
```

**Missing protections:**
1. **`is_under_construction` check**: Partially initialized objects could be traced prematurely
2. **Second `is_allocated` re-check before `push_work`**: Slot could be swept between generation check and push, leading to dangling pointer

### Comparable Functions with Proper Protection

| Function | is_allocated #1 | generation | generation_recheck | is_under_construction | is_allocated #2 | Action |
|----------|-----------------|------------|-------------------|----------------------|-----------------|--------|
| `worker_mark_loop` (marker.rs:908) | Line 909 | Line 917 | Line 922 | **Line 928** | **Line 935** | `trace_fn` |
| `scan_page_for_unmarked_refs` (incremental.rs:1045) | Line 1056 | Line 1062 | Line 1081 | **Line 1088** | **Lines 1067, 1094** | `push_work` |
| `mark_and_push_to_worker_queue` (gc.rs:1225) | Line 1226 | Line 1230 | Line 1231 | **MISSING** | **MISSING** | `push_work` |

---

## 🔬 根本原因分析 (Root Cause Analysis)

**TOCTOU Race Scenario:**

1. Thread A successfully marks an object at `gc_box` with generation 5
2. Thread A passes all checks (is_allocated=true, generation matches)
3. Lazy sweeper deallocates the slot (is_allocated=false)
4. Lazy sweeper reallocates the slot with a new object (generation=6)
5. Thread A pushes `gc_box` to worker queue (the pointer is now dangling!)
6. Worker tries to trace the reallocated object incorrectly

**Partially Initialized Object Scenario:**

1. `Gc::new_cyclic_weak` allocates a slot and starts construction
2. Before construction completes, marking thread encounters the object
3. Object is traced before `is_under_construction` is cleared
4. `trace_fn` may access uninitialized fields or follow invalid pointers

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Theoretical race requiring precise concurrent timing:
1. Allocate object A in slot with generation G
2. Object A becomes unreachable
3. Parallel marking starts, `gc_box` obtained for object A
4. `try_mark` succeeds
5. Lazy sweep deallocates slot and reallocates with object B (generation G+1)
6. Generation check passes (G != G+1), mark is NOT cleared
7. Object B is pushed to worker queue and traced incorrectly

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

After line 1234 and before line 1242, add:

```rust
// FIX bug570: Check is_under_construction to avoid tracing partially initialized objects
if (*gc_box.as_ptr()).is_under_construction() {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}

// FIX bug570: Second is_allocated re-check to fix TOCTOU with lazy sweep
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The mark-then-trace pattern in concurrent GC is inherently susceptible to TOCTOU races. Chez Scheme uses a different approach where sweep and mark are fully serialized. For concurrent GC, the fix pattern is correct: verify slot liveness after successful mark and before any operation that uses the slot pointer.

**Rustacean (Soundness 觀點):**
The missing `is_under_construction` check is a soundness concern. Partially initialized objects should never be traced. The second `is_allocated` check prevents TOCTOU where a slot could be swept and reused between our checks and the `push_work` call.

**Geohot (Exploit 觀點):**
While the race window is small, an attacker who could influence GC timing might trigger the TOCTOU scenario. The dangling pointer pushed to the worker queue could lead to incorrect object tracing, potentially causing memory corruption or information disclosure.

---

## 🔗 相關 Issue

- bug360: mark_and_push_to_worker_queue missing generation check (Fixed)
- bug558: mark_and_push_to_worker_queue has same pattern as bug551 (Fixed)
- bug469: Skip objects under construction in worker_mark_loop
- bug529: worker_mark_loop TOCTOU with lazy sweep (second is_allocated check)
- bug509: scan_page_for_unmarked_refs missing second is_allocated check
