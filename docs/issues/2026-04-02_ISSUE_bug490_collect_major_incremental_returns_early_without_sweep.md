# [Bug]: collect_major_incremental returns early without sweeping when fallback has remaining work

**Status:** Open
**Tags:** Verified, Regression

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | When incremental marking fallback occurs with remaining work |
| **Severity (嚴重程度)** | High | Memory leak - dead objects accumulate |
| **Reproducibility (復現難度)** | Medium | Requires specific incremental marking workload |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `collect_major_incremental` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `execute_final_mark` is called after a fallback and sets phase to `Marking` (because `remaining > 0`), the function should either:
1. Continue marking until worklist is drained, OR
2. Perform a full STW sweep before returning

### 實際行為 (Actual Behavior)
At lines 1851-1861 in `collect_major_incremental`:
```rust
timer.start();
let reclaimed = if state.phase() == MarkPhase::Sweeping {
    sweep_segment_pages(heap, false)
} else {
    0
};
let reclaimed_large = if state.phase() == MarkPhase::Sweeping {
    sweep_large_objects(heap, false)
} else {
    0
};
```

When `execute_final_mark` sets phase to `Marking` (due to remaining work), `reclaimed` and `reclaimed_large` are both `0` **without any sweep occurring**. Dead objects that were marked during the initial marking phase are never reclaimed. The function returns with `objects_reclaimed: 0`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `collect_major_incremental` (gc/gc.rs:1805-1874):

1. Lines 1828-1833: When `MarkSliceResult::Fallback` occurs, sets `state.set_phase(MarkPhase::FinalMark)` and breaks from loop

2. Lines 1838-1849: After the loop:
   - Gets `remaining = state.worklist_len()`
   - Gets `dirty_pages = count_dirty_pages(heap)`
   - If `remaining > 0 || dirty_pages > 0`:
     - Calls `execute_final_mark(heaps_mut)`
     - `execute_final_mark` may set phase to `Marking` if remaining > 0
   - Else: sets phase to `Sweeping`

3. Lines 1851-1861: **BUG** - If phase is NOT `Sweeping`, `reclaimed` and `reclaimed_large` are both `0` without any sweep occurring

The issue is that when fallback occurs and there's remaining work, `execute_final_mark` sets phase to `Marking`, and then the conditional sweep returns 0 without sweeping any objects.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Trigger conditions:
// 1. Start an incremental major GC
// 2. Create workload with objects only reachable through chains requiring multiple passes
// 3. Trigger fallback (e.g., dirty pages exceeded)
// 4. execute_final_mark finds remaining work and sets phase to Marking
// 5. collect_major_incremental returns without sweeping
// 6. Dead objects accumulate - memory leak
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

When phase is not `Sweeping` after `execute_final_mark`, we should still sweep the dead objects we already marked. The sweep will only reclaim objects that were marked as dead (unreachable), so it's safe to sweep even if marking is incomplete.

```rust
if state.phase() != MarkPhase::Sweeping {
    // Remaining work exists, but we should still sweep dead objects
    // that were already marked. Objects that are still reachable
    // (marked in this cycle) will have their mark bits cleared
    // in the next cycle.
    //
    // Note: We don't promote pages here since marking isn't complete.
}
```

Actually, looking more carefully, if `remaining > 0` after `execute_final_mark`, it means there are objects that couldn't be fully processed. In this case, we should either:

1. **Option A**: Force sweep anyway (objects that were marked dead will still be swept)
2. **Option B**: Return with 0 reclaimed but properly transition to Idle

The issue is that returning with `objects_reclaimed: 0` when we've already done marking work is misleading - the user thinks no GC happened when in fact dead objects weren't reclaimed.

A better fix might be to still perform sweep even when phase is Marking:

```rust
if state.phase() != MarkPhase::Sweeping {
    // If fallback occurred but we have marked objects, still try to sweep
    // Only sweep what we know is dead (marked objects)
    // The next GC cycle will handle remaining work
}
timer.start();
let reclaimed = sweep_segment_pages(heap, false);
let reclaimed_large = sweep_large_objects(heap, false);
// Don't promote pages if marking wasn't complete
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The bug violates the complete marking promise. When we fall back, we should still reclaim what we can. Objects that were marked as unreachable in earlier marking passes should still be swept even if new work was discovered.

**Rustacean (Soundness 觀點):**
This is a memory leak, not unsoundness. The marked objects that weren't swept will be re-scanned in the next GC cycle. However, the `objects_reclaimed: 0` return value is misleading.

**Geohot (Exploit 觀點):**
Not directly exploitable but can cause memory pressure over time if GC fallback occurs frequently.

---

## 🔄 回歸記錄 (Regression Record)

**2026-04-02**: Bug490 was fixed in commit `838c448` but regressed in commit `d5b39f0` (fix for bug488).

**Root cause of regression**: 
Commit `d5b39f0` "fix(gc): only sweep when phase is Sweeping to prevent USE-AFTER-FREE" was necessary to fix bug488 (USE-AFTER-FREE when sweeping during Marking phase). However, it changed the code from unconditionally sweeping to only sweeping when `phase == MarkPhase::Sweeping`.

This reintroduced bug490's memory leak because when fallback occurs and `execute_final_mark` sets phase to `Marking`, no sweep happens.

**Trade-off**:
- Sweeping when phase is `Marking` → USE-AFTER-FREE (safety issue - bug488)
- Not sweeping when phase is `Marking` → Memory leak (bug490)

**Proper fix needed**: A solution that prevents USE-AFTER-FREE while still reclaiming dead objects. Options:
1. Only sweep objects that were confirmed dead before the fallback
2. Use a two-phase approach: mark-dead-then-sweep instead of mark-all-then-sweep
3. Track which objects were marked in the current cycle vs previous cycles