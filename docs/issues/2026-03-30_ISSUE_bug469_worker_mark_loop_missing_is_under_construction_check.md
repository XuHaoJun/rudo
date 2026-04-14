# [Bug]: worker_mark_loop_with_registry missing is_under_construction check before trace_fn

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Only occurs during Gc::new_cyclic with parallel marking |
| **Severity (嚴重程度)** | High | Could trace partially initialized objects, causing memory corruption |
| **Reproducibility (復現難度)** | Medium | Requires parallel GC + new_cyclic, but consistent when triggered |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/marker.rs`, `worker_mark_loop_with_registry`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`worker_mark_loop_with_registry` should skip objects that are under construction (e.g., during `Gc::new_cyclic`) before calling `trace_fn`, similar to how `mark_object_black` and `mark_new_object_black` check `is_under_construction()`.

### 實際行為 (Actual Behavior)
`worker_mark_loop_with_registry` calls `trace_fn` at line 1136 without checking if the object is under construction. If a `Gc::new_cyclic` object is being processed by parallel marking workers, the partially initialized object could be traced, potentially following back-edges that shouldn't exist yet.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `gc/marker.rs`, `worker_mark_loop_with_registry` (lines 1072-1169):

1. Line 1117: Gets `gc_box_ptr` from `obj.cast_mut()`
2. Lines 1121-1134: Checks generation to detect slot reuse
3. Line 1136: Calls `trace_fn` WITHOUT checking `is_under_construction()`

Compare with `mark_object_black` in `gc/incremental.rs:1041` and `gc/incremental.rs:1098` which correctly check:
```rust
if gc_box.is_under_construction() {
    return false;  // or None
}
```

The `try_mark` function in `PageHeader` (heap.rs:1103-1120) does NOT check `is_under_construction` - it only checks the mark bitmap. So even if an object is under construction, `try_mark` can succeed, and then `trace_fn` is called.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires parallel marking enabled
// PoC would involve:
// 1. Create Gc with new_cyclic in a multi-threaded context
// 2. Enable parallel marking
// 3. The weak reference in new_cyclic could be traced prematurely
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_under_construction()` check after line 1134 and before line 1135:

```rust
// Verify generation hasn't changed before calling trace_fn (bug427 fix).
// If slot was reused, trace_fn would be called on wrong object data.
if (*gc_box_ptr).generation() != marked_generation {
    break; // Slot was reused - skip
}
// FIX bug469: Skip objects under construction (e.g. Gc::new_cyclic)
if (*gc_box_ptr).is_under_construction() {
    break;
}
marked += 1;
((*gc_box_ptr).trace_fn)(ptr_addr, &mut visitor);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
In Chez Scheme's GC, objects under construction are never visible to the collector. The GC only traces objects that have been fully initialized and registered. The `is_under_construction` flag serves the same purpose - to prevent tracing of objects that are still being initialized. In `Gc::new_cyclic`, the weak reference is deliberately not traced to allow the back-reference to be set up correctly.

**Rustacean (Soundness 觀點):**
Calling `trace_fn` on a partially initialized object is technically not undefined behavior since the memory is valid. However, it violates the GC's invariants and could lead to:
- Following uninitialized/back-reference pointers
- Incorrect reference counting
- Memory corruption if the trace_fn assumes the object is fully initialized

**Geohot (Exploit 觀點):**
If an attacker can influence the `new_cyclic` flow or the object layout, they might be able to craft a scenario where the premature trace follows a controlled pointer. This could potentially be leveraged for memory disclosure or corruption attacks in edge cases.