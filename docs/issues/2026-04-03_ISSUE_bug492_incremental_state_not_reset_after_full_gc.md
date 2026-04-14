# [Bug]: IncrementalMarkState not reset after perform_multi_threaded_collect_full() causes stale state

**Status:** Open
**Tags:** Unverified

## Threat Model Assessment

| Metric | Level | Description |
| :--- | :--- | :--- |
| **Likelihood** | Medium | When collect_full() runs while incremental GC is in progress |
| **Severity** | Medium | Incremental GC resumes with stale state, potential incorrect behavior |
| **Reproducibility** | Low | Requires specific timing of concurrent GC requests |

---

## Affected Component & Environment
- **Component:** `perform_multi_threaded_collect_full()` (gc/gc.rs:849-1028)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior

After a full GC completes in `perform_multi_threaded_collect_full()`, the `IncrementalMarkState` should be reset to `Idle` to ensure subsequent incremental GCs start from a clean state.

### Actual Behavior

At the end of `perform_multi_threaded_collect_full()` (lines 1018-1027):

```rust
crate::heap::resume_all_threads();
crate::heap::clear_gc_request();

// CRITICAL FIX: Clear global gc_in_progress flag after GC completes
crate::heap::thread_registry()
    .lock()
    .unwrap()
    .set_gc_in_progress(false);

IN_COLLECT.with(|in_collect| in_collect.set(false));
```

**Missing**: `IncrementalMarkState::global().set_phase(MarkPhase::Idle)` is NOT called.

When `perform_multi_threaded_collect_full()` completes and resumes threads, the `IncrementalMarkState` retains whatever phase it had before the collection started. If an incremental GC was in progress (phase = Marking, FinalMark, etc.), the state is now stale.

### Scenario

1. Thread A is running incremental GC, `IncrementalMarkState::phase() = Marking`
2. Thread B calls `collect_full()`, becomes collector via `request_gc_handshake()`
3. Thread A enters safepoint and waits
4. Thread B runs `perform_multi_threaded_collect_full()` - Clear → Mark → Sweep
5. Thread B calls `resume_all_threads()` - Thread A resumes
6. **Bug**: `IncrementalMarkState::phase()` is still `Marking` despite full GC completing
7. When incremental GC continues, it sees stale state

### Comparison with `collect_major_incremental()`

`collect_major_incremental()` correctly handles state transitions:
- Calls `execute_final_mark()` which sets phase appropriately
- Calls `sweep_segment_pages()` only when `phase == MarkPhase::Sweeping`

But `perform_multi_threaded_collect_full()` does NOT interact with `IncrementalMarkState` at all, leaving it stale.

---

## Root Cause Analysis

In `perform_multi_threaded_collect_full()` (gc/gc.rs:849-1028):

1. Line 878-883: Sets `gc_in_progress = true`
2. Lines 896-914: Phase 1 - Clear marks
3. Lines 930-940: Phase 2 - Mark objects
4. Lines 956-964: Phase 3 - Sweep ALL heaps
5. Lines 1018-1027: Cleanup - resumes threads, clears gc_in_progress

**Missing**: No interaction with `IncrementalMarkState` during the entire process.

The function runs a complete stop-the-world GC (Clear → Mark → Sweep) but treats `IncrementalMarkState` as if it doesn't exist. This is correct ONLY if no incremental GC was in progress. But if an incremental GC WAS in progress, the state becomes stale.

---

## Suggested Fix

Add `IncrementalMarkState::reset()` or `set_phase(MarkPhase::Idle)` at the end of `perform_multi_threaded_collect_full()`:

```rust
// At line 1027, after IN_COLLECT.set(false):
IncrementalMarkState::global().set_phase(MarkPhase::Idle);
```

Or call `IncrementalMarkState::global().reset()` to fully reset the state.

---

## Internal Discussion Record

**R. Kent Dybvig (GC Architecture Perspective):**
The incremental GC state machine relies on `IncrementalMarkState::phase()` to determine what work remains. If a stop-the-world GC runs and doesn't update this state, subsequent incremental work could be based on incorrect assumptions. The full GC effectively "completes" the incremental GC's work, so the state should reflect that.

**Rustacean (Soundness Perspective):**
Not resetting the phase is not undefined behavior, but could lead to logic errors. The incremental GC might skip necessary work or perform unnecessary work if the phase doesn't match reality.

**Geohot (Exploit Perspective):**
The stale state could potentially be exploited if an attacker can control GC timing. However, the window is small and the impact is limited to GC correctness issues.
