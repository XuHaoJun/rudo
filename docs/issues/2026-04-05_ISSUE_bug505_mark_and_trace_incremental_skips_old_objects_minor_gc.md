# [Bug]: mark_and_trace_incremental skips marking old objects during minor GC in dirty pages

**Status:** Open
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any minor GC with dirty old objects referencing young objects |
| **Severity (嚴重程度)** | Critical | Use-after-free: young object collected while still referenced by old |
| **Reproducibility (復現難度)** | Medium | Requires specific mutation pattern; full GC masks the bug |

---

## 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_and_trace_incremental` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 問題描述 (Description)

During minor GC, `scan_dirty_page_minor_trace` iterates over dirty objects to find OLD→YOUNG references. It calls `mark_and_trace_incremental` for each dirty object. However, `mark_and_trace_incremental` has an early return at lines 2442-2446 that skips old objects entirely during minor GC:

```rust
if visitor.kind == VisitorKind::Minor
    && (*header.as_ptr()).generation.load(Ordering::Acquire) > 0
{
    return;  // BUG: Returns BEFORE marking and tracing
}
```

This causes OLD→YOUNG references in dirty pages to be ignored, potentially causing young objects to be collected prematurely.

### 預期行為 (Expected Behavior)
Old objects in dirty pages should be marked (to prevent collection) and traced (to find young references) during minor GC when called from `scan_dirty_page_minor_trace`.

### 實際行為 (Actual Behavior)
`mark_and_trace_incremental` returns early for old objects during minor GC without marking or tracing them.

---

## 根本原因分析 (Root Cause Analysis)

In `mark_and_trace_incremental` (gc.rs:2433-2491):
1. Lines 2441-2446 check if `visitor.kind == VisitorKind::Minor && generation > 0`
2. If true, the function returns **before** the marking loop (lines 2454-2484) and **before** the worklist push (line 2490)
3. This is inconsistent with `mark_object_minor` (lines 2114-2116) which marks old objects but skips worklist push

The dirty page mechanism exists specifically to capture OLD→YOUNG mutations. By skipping old objects entirely, those references are never traced and young objects may be incorrectly collected.

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Create an old object with a `GcCell` field pointing to a young object
2. Promote the old object to old generation (via `collect_full()`)
3. Mutate the `GcCell` to point to a new young object (creates OLD→YOUNG reference, marks page dirty)
4. Run minor GC (`collect()`) - the young object may be incorrectly collected

Note: Full GC masks this bug because it traces from roots regardless of barrier.

---

## 建議修復方案 (Suggested Fix / Remediation)

Remove the early return at lines 2442-2446 in `mark_and_trace_incremental`. The function should mark old objects and trace them. The generation check should only skip the worklist push (like `mark_object_minor` does).

```rust
// Remove lines 2441-2446:
// if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
//     if visitor.kind == VisitorKind::Minor
//         && (*header.as_ptr()).generation.load(Ordering::Acquire) > 0
//     {
//         return;  // REMOVE THIS
//     }
// }

// Instead, after the marking loop (line 2484), add generation check before worklist push:
let enqueue_generation = (*ptr.as_ptr()).generation();
if visitor.kind != VisitorKind::Minor || enqueue_generation == 0 {
    visitor.worklist.push((ptr, enqueue_generation));
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The dirty page mechanism is crucial for generational GC. When an OLD→YOUNG reference is created, the page is marked dirty so minor GC can scan it. The `mark_and_trace_incremental` function is called from `scan_dirty_page_minor_trace` specifically to handle this case. Returning early defeats the entire purpose of dirty page tracking. This is a fundamental flaw in incremental marking's handling of generational barriers.

**Rustacean (Soundness 觀點):**
If a young object is collected due to this bug, any subsequent access to that object (or its fields via GcCell) would be a use-after-free. The `GcCell::borrow()` would return `None` even though the old object still "points" to it. This is a memory safety violation.

**Geohot (Exploit 觀點):**
An attacker could exploit this by:
1. Allocating sensitive data in a young object
2. Creating an OLD→YOUNG reference to it
3. Causing minor GC to collect the young object
4. Reallocating the memory and reading stale data

The attack surface is limited but the consequences are severe.
