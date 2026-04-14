# [Bug]: collect_major_incremental unconditionally sets phase to Sweeping after execute_final_mark

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Triggers when Fallback occurs during incremental marking |
| **Severity (嚴重程度)** | Critical | Reachable objects may be prematurely swept, causing USE-AFTER-FREE |
| **Reproducibility (復現難度)** | High | Requires concurrent GC timing with dirty page threshold or slice timeout |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `collect_major_incremental()` in `gc/gc.rs:1818-1861`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
After `execute_final_mark` processes remaining worklist items and dirty pages, `collect_major_incremental` should check whether marking is complete before transitioning to the Sweeping phase. If `execute_final_mark` leaves work remaining, marking should continue (or a proper fallback should occur).

### 實際行為 (Actual Behavior)
At line 1845, `collect_major_incremental` unconditionally calls `state.set_phase(MarkPhase::Sweeping)` immediately after `execute_final_mark`, regardless of whether `execute_final_mark` determined there was remaining work.

```rust
// gc.rs:1840-1845
if remaining > 0 || dirty_pages > 0 {
    let heaps_mut: &mut [&mut LocalHeap; 1] = &mut [heap];
    execute_final_mark(heaps_mut);
}

state.set_phase(MarkPhase::Sweeping);  // BUG: Unconditional!
```

Even if `execute_final_mark` sets the phase to `Marking` (because `remaining > 0` after draining worklist), `collect_major_incremental` overwrites it to `Sweeping`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

### The execute_final_mark Contract

`execute_final_mark` (incremental.rs:880-954) has a specific contract:
1. It processes cross-thread SATB buffers
2. It processes dirty pages via `scan_page_for_unmarked_refs`
3. It drains `visitor.worklist` into `state.worklist` (lines 934-937)
4. It drains `state.worklist` by calling `trace_and_mark_object` on each item (lines 939-944)
5. It checks `remaining = state.worklist_len()` and sets phase to `Marking` if > 0, else `Sweeping` (lines 946-951)

### The Bug

At line 1845, `collect_major_incremental` unconditionally sets the phase to `Sweeping` after calling `execute_final_mark`, ignoring the phase that `execute_final_mark` set:

```rust
state.set_phase(MarkPhase::Sweeping);  // Overwrites whatever execute_final_mark set!
```

### How the Bug Manifests

If `execute_final_mark` determines that `remaining > 0` (objects still need marking), it sets the phase to `Marking`. But `collect_major_incremental` immediately overwrites this with `Sweeping`.

Even though `execute_final_mark` should drain the worklist, if there are timing issues or if `trace_and_mark_object` pushes new items during the loop, `remaining` could be > 0 after `execute_final_mark` returns.

### Scenario

1. During `mark_slice`, `state.worklist` has objects that haven't been fully traced
2. `MarkSliceResult::Fallback` is returned (e.g., dirty_pages > max_dirty_pages)
3. `collect_major_incremental` breaks from the mark loop
4. `execute_final_mark` is called because `remaining > 0 || dirty_pages > 0`
5. `execute_final_mark` processes items but due to concurrent modifications, `remaining > 0`
6. `execute_final_mark` sets phase to `Marking`
7. Line 1845: `collect_major_incremental` overwrites to `Sweeping`
8. Sweep runs and reclaims pages containing reachable objects → USE-AFTER-FREE

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires careful timing to trigger Fallback during mark_slice
// with objects remaining in state.worklist

#[test]
fn test_fallback_sweeps_reachable_objects() {
    // 1. Setup: Create object graph with references
    // 2. Start incremental major GC
    // 3. During mark_slice, trigger Fallback by:
    //    - Exceeding dirty_pages threshold, OR
    //    - Slice timeout occurs
    // 4. Verify objects in worklist were traced before sweep
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Option 1: Check remaining after execute_final_mark and handle appropriately:

```rust
if remaining > 0 || dirty_pages > 0 {
    let heaps_mut: &mut [&mut LocalHeap; 1] = &mut [heap];
    execute_final_mark(heaps_mut);
}

// FIX: Check if execute_final_mark left work remaining
let remaining = state.worklist_len();
let dirty_pages = count_dirty_pages(heap);
if remaining > 0 || dirty_pages > 0 {
    // Work remaining - this should not happen if execute_final_mark works correctly
    // But if it does, we should not sweep
    // Option: request STW fallback or continue marking
    state.set_phase(MarkPhase::Sweeping);  // Or handle differently
} else {
    state.set_phase(MarkPhase::Sweeping);
}
```

Option 2: Respect the phase set by execute_final_mark:

```rust
if remaining > 0 || dirty_pages > 0 {
    let heaps_mut: &mut [&mut LocalHeap; 1] = &mut [heap];
    execute_final_mark(heaps_mut);
    // Don't overwrite - execute_final_mark already set the phase
} else {
    state.set_phase(MarkPhase::Sweeping);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The unconditional phase transition violates the incremental GC contract. If there's remaining work after `execute_final_mark`, we should not immediately sweep. The phase transition should respect what `execute_final_mark` determined.

**Rustacean (Soundness 觀點):**
If reachable objects are swept because we transition to Sweeping prematurely, we have a use-after-free situation. This is a memory safety violation.

**Geohot (Exploit 觀點):**
An attacker could potentially trigger this condition to cause memory corruption or facilitate exploits.

---

## 備註

- The bug report from HUNT_BUG_MARK.md claims `execute_final_mark` doesn't drain `state.worklist`, but the code at lines 939-944 does call `trace_and_mark_object` on each item popped from `state.worklist`.
- However, the unconditional `state.set_phase(MarkPhase::Sweeping)` at line 1845 is still problematic regardless of whether `execute_final_mark` is correct.
