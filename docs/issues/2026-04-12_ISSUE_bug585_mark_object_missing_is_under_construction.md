# [Bug]: mark_object missing is_under_construction check

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires cross-thread SATB buffer processing during Gc::new_cyclic_weak |
| **Severity (嚴重程度)** | High | May trace partially initialized object during cyclic weak construction |
| **Reproducibility (復現難度)** | Medium | Concurrent incremental marking needed, stress tests can trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object` (gc/gc.rs:2424-2479)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`mark_object()` should skip objects that are under construction (e.g., during `Gc::new_cyclic_weak`) before marking and enqueueing, consistent with all other marking functions.

### 實際行為 (Actual Behavior)
`mark_object()` checks `is_allocated` and `generation`, but does **NOT** check `is_under_construction()` before pushing to worklist. This allows partially constructed objects to be traced.

**Contrast with `mark_and_trace_incremental` (lines 2524-2529):**
```rust
// FIX bug547: Skip objects under construction (e.g. Gc::new_cyclic).
// Matches mark_object_minor (bug546), worker_mark_loop (bug469), mark_object_black (bug238).
if (*ptr.as_ptr()).is_under_construction() {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
visitor.objects_marked += 1;
```

`mark_object` at lines 2448-2462 is missing this check.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `mark_object()` (gc/gc.rs:2448-2463):

```rust
Ok(true) => {
    // FIX bug559: Check is_allocated FIRST to avoid UB.
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    let marked_generation = (*ptr.as_ptr()).generation();
    if (*ptr.as_ptr()).generation() != marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // MISSING: is_under_construction() check!
    visitor.objects_marked += 1;
    break;
}
```

The code correctly handles:
- `is_allocated` check (prevents UB from reading deallocated slot)
- `generation` check (detects slot reuse)

But it **forgot** to add the `is_under_construction()` check that exists in similar functions.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

The bug manifests when:
1. Thread A is creating a `Gc::new_cyclic_weak()` - object is `is_under_construction()=true`
2. Thread B mutates a shared GC object pointing to Thread A's new object
3. Cross-thread SATB records this pointer
4. Final mark phase calls `mark_object()` on the pointer
5. Object under construction is marked and later traced incorrectly

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add the `is_under_construction()` check after the generation check and before `visitor.objects_marked += 1`:

```rust
Ok(true) => {
    // FIX bug559: Check is_allocated FIRST to avoid UB.
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    let marked_generation = (*ptr.as_ptr()).generation();
    if (*ptr.as_ptr()).generation() != marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    // FIX bug585: Skip objects under construction (e.g. Gc::new_cyclic).
    // Matches mark_and_trace_incremental (bug547), mark_object_minor (bug546),
    // worker_mark_loop (bug469), mark_object_black (bug238).
    if (*ptr.as_ptr()).is_under_construction() {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`mark_object` is called from `execute_final_mark()` in incremental.rs during cross-thread SATB buffer processing. If a partially constructed object (during `Gc::new_cyclic_weak`) is in the SATB buffer, it will be incorrectly marked and traced. This is inconsistent with all other marking paths which check `is_under_construction()`.

**Rustacean (Soundness 觀點):**
Tracing an object under construction could call `trace_fn` on uninitialized memory, leading to potential UB. The fix is straightforward - add the same `is_under_construction()` check that exists in `mark_and_trace_incremental`.

**Geohot (Exploit 觀點):**
While this bug requires specific timing (concurrent `Gc::new_cyclic_weak` with cross-thread mutation during incremental final mark), the consequence is serious - partially initialized object data could be traversed by the GC tracer.