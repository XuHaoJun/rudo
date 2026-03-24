# [Bug]: collect_full Paths Missing GC_MARK_IN_PROGRESS Flag

**Status:** Fixed
**Tags:** Bug, Fixed

## Threat Model Assessment

| Assessment | Rating | Description |
| :--- | :--- | :--- |
| **Likelihood** | Medium | Requires concurrent allocation during incremental marking in `collect_full` path |
| **Severity** | Medium | Could cause newly-allocated objects to be incorrectly marked as reachable |
| **Reproducibility** | High | Can be triggered reliably with concurrent allocation during `collect_full` |

---

## Affected Component & Environment

- **Component:** `perform_multi_threaded_collect_full`, `perform_single_threaded_collect_full` in `crates/rudo-gc/src/gc/gc.rs`
- **Feature:** lazy-sweep with `alloc_from_pending_sweep`
- **OS / Architecture:** All
- **Rust Version:** 1.75+

---

## Problem Description

### Expected Behavior

During a major collection's mark phase, `alloc_from_pending_sweep` should not allocate from pages pending sweep. The `GC_MARK_IN_PROGRESS` flag is used to signal that marking is in progress, allowing `alloc_from_pending_sweep` to return `None` and fall back to other allocation paths.

### Actual Behavior

`perform_multi_threaded_collect` (the normal multi-threaded major collection path) correctly sets `GC_MARK_IN_PROGRESS` around its mark phase:

```rust
// gc.rs lines 422 and 431
super::sync::GC_MARK_IN_PROGRESS.store(true, std::sync::atomic::Ordering::Release);
for tcb in &tcbs {
    // ... marking code ...
}
super::sync::GC_MARK_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Release);
```

However, `perform_multi_threaded_collect_full` and `perform_single_threaded_collect_full` do NOT set this flag at all, despite performing marking.

---

## Root Cause Analysis

In `alloc_from_pending_sweep` (heap.rs:2162-2200):

```rust
fn alloc_from_pending_sweep(&mut self, class_index: usize) -> Option<NonNull<u8>> {
    if crate::gc::sync::GC_MARK_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
        return None;  // BUG: GC_MARK_IN_PROGRESS not set in collect_full paths
    }
    // ... allocates from pending sweep pages ...
}
```

**Problem scenario in `perform_single_threaded_collect_full`:**

1. `collect_full()` calls `wake_waiting_threads()` to wake other threads (line 623)
2. `perform_single_threaded_collect_full()` is called (line 624)
3. `collect_major(heap)` is called (line 792), which may use incremental marking
4. If incremental, `execute_snapshot` stops mutators, marks roots, resumes mutators
5. Then `mark_slice` runs in a loop with mutators RUNNING and allocating
6. `GC_MARK_IN_PROGRESS` is NEVER set in this path
7. `alloc_from_pending_sweep` succeeds during marking
8. Dead object is swept, slot is reclaimed
9. New object is allocated in same slot - but mark bitmap may have stale info

---

## Suggested Fix

Add `GC_MARK_IN_PROGRESS` setting in both `perform_multi_threaded_collect_full` and `perform_single_threaded_collect_full` around their mark phases, consistent with `perform_multi_threaded_collect`.

For `perform_multi_threaded_collect_full`, add before mark phase (around line 944):
```rust
super::sync::GC_MARK_IN_PROGRESS.store(true, std::sync::atomic::Ordering::Release);
```

And after mark phase (around line 953):
```rust
super::sync::GC_MARK_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Release);
```

For `perform_single_threaded_collect_full`, the marking happens inside `collect_major`, so the fix would need to either:
1. Add `GC_MARK_IN_PROGRESS` in `collect_major_stw` and around incremental marking loops, OR
2. Set `GC_MARK_IN_PROGRESS` before calling `collect_major` in `perform_single_threaded_collect_full`

---

## Related Bugs

- Bug336: Incremental marking TOCTOU - Lazy Sweep Reallocation (similar area, different bug)
- Bug329: Lazy Sweep concurrent alloc all_dead flag issue (similar area, different bug)

---

## Verification

The `GC_MARK_IN_PROGRESS` flag is only set in ONE place in the entire codebase:
- `crates/rudo-gc/src/gc/gc.rs` lines 422 and 431 (in `perform_multi_threaded_collect`)

It is NOT set in:
- `perform_multi_threaded_collect_full` (lines 863-1040)
- `perform_single_threaded_collect_full` (lines 781-854)
- `collect_major_stw` (lines 1769-1818)
- `collect_major_incremental` (lines 1821-1900)
