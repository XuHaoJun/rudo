# [Bug]: mark_root_for_snapshot pushes redundant worklist entries when object is already marked

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Occurs during incremental marking when roots are scanned multiple times or when snapshot phase runs concurrently with ongoing marking |
| **Severity (嚴重程度)** | Low | Performance inefficiency only - does not affect correctness since downstream processing checks is_marked |
| **Reproducibility (復現難度)** | Low | Code inspection shows the issue clearly; could be observed via worklist size metrics |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (`mark_root_for_snapshot` in `gc/incremental.rs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `mark_root_for_snapshot` encounters an already-marked object, it should return early without pushing to the worklist, similar to how `GcVisitor::visit()` handles already-marked objects.

### 實際行為 (Actual Behavior)
The function checks if the object was already marked (`was_marked = is_marked(idx)`), but unconditionally pushes to the worklist regardless of the result. This creates redundant worklist entries for objects that have already been marked and processed.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `crates/rudo-gc/src/gc/incremental.rs:575-580`:

```rust
let was_marked = (*header.as_ptr()).is_marked(idx);
if !was_marked {
    (*header.as_ptr()).set_mark(idx);
    visitor.objects_marked += 1;
}
visitor.worklist.push(ptr);  // BUG: Always pushes, even when was_marked is true
```

Compare with `GcVisitor::visit()` in `gc/gc.rs:3006-3008`:

```rust
if (*header.as_ptr()).is_marked(idx) {
    return;  // Returns early without pushing
}
```

The `mark_root_for_snapshot` function has the correct pattern for setting the mark (only set if not already marked), but fails to apply the same logic to worklist pushing.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This is a code inspection bug. The issue can be observed by:
1. Running incremental marking with multiple snapshot phases
2. Observing worklist size growing despite no new objects being marked

```rust
// Conceptual: In a scenario with concurrent marking,
// if mark_root_for_snapshot is called on an already-marked root,
// it will unnecessarily add it to the worklist again.
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the `visitor.worklist.push(ptr)` inside the `if !was_marked` block:

```rust
let was_marked = (*header.as_ptr()).is_marked(idx);
if !was_marked {
    (*header.as_ptr()).set_mark(idx);
    visitor.objects_marked += 1;
    visitor.worklist.push(ptr);  // Only push when we actually marked it
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The SATB (Snapshot-At-The-Beginning) algorithm requires all roots to be captured at snapshot time. However, if a root is already marked from a previous incremental marking phase, pushing it again to the worklist creates redundant processing. This is inefficient but does not affect correctness because `process_worklist` also checks `is_marked` before processing.

**Rustacean (Soundness 觀點):**
No soundness issues. The code is safe and the redundant worklist entries are handled correctly downstream. This is purely a performance optimization opportunity.

**Geohot (Exploit 觀點):**
No exploit potential. The redundant worklist entries could theoretically increase memory pressure in high-throughput scenarios, but this is not a security concern.

---

## Resolution (2026-03-15)

Fixed by moving `visitor.worklist.push(ptr)` inside the `if !was_marked` block in `mark_root_for_snapshot` (incremental.rs). Only roots that were actually marked are pushed to the worklist, matching `GcVisitor::visit()` behavior. Tests `test_execute_snapshot_captures_roots` and `test_root_capture_with_nested_objects` were updated to run `collect_full()` before `execute_snapshot` so roots are unmarked (objects allocated via `Gc::new` are pre-marked by `mark_new_object_black`).
