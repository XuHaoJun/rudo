# [Bug]: Incremental marking fallback abandons state.worklist causing reachable objects to be swept

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | Incremental marking hits fallback frequently under memory pressure |
| **Severity (嚴重程度)** | `Critical` | Use-after-free of reachable GC objects |
| **Reproducibility (復現難度)** | `High` | Trigger by dirty_pages > max_dirty_pages or slice_timeout_ms |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (`collect_major_incremental`, `execute_final_mark`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0 (incremental marking feature)

---

## 📝 問題描述 (Description)

When `mark_slice` returns `Fallback` (due to `dirty_pages > max_dirty_pages` or `slice_elapsed > slice_timeout_ms`), the `collect_major_incremental` function unconditionally transitions to `Sweeping` phase without processing remaining items in `state.worklist`.

### 預期行為 (Expected Behavior)
- When Fallback occurs with remaining work in `state.worklist`, all marked objects should be fully traced before sweep begins
- OR the remaining work should be processed before transitioning to Sweeping

### 實際行為 (Actual Behavior)
- `execute_final_mark` only processes `visitor.worklist` (SATB items) and drains them to `state.worklist`
- `collect_major_incremental` unconditionally sets phase to `Sweeping` (line 1845), **overwriting** the phase decision made by `execute_final_mark`
- Objects in `state.worklist` that weren't processed via `trace_and_mark_object` have their children never traced
- Sweep reclaims these reachable objects → **USE-AFTER-FREE**

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Location:** `crates/rudo-gc/src/gc/gc.rs:1845`

```rust
// collect_major_incremental (lines 1829-1845):
MarkSliceResult::Fallback { reason } => {
    log_fallback_reason(reason);
    state.set_phase(MarkPhase::FinalMark);
    break;  // Exit mark loop
}
// ...
let remaining = state.worklist_len();
let dirty_pages = count_dirty_pages(heap);
if remaining > 0 || dirty_pages > 0 {
    execute_final_mark(heaps_mut);  // Only drains visitor.worklist -> state.worklist
}

state.set_phase(MarkPhase::Sweeping);  // BUG: Unconditional, ignores remaining work!
```

**`execute_final_mark` (incremental.rs:929-939):**
```rust
while let Some((ptr, _enqueue_generation)) = visitor.worklist.pop() {
    state.push_work(ptr);  // Only processes SATB items, NOT state.worklist items!
    total_marked += 1;
}
let remaining = state.worklist_len();
if remaining > 0 {
    state.set_phase(MarkPhase::Marking);  // This is IGNORED by caller!
} else {
    state.set_phase(MarkPhase::Sweeping);
}
```

The bug is that `collect_major_incremental` **unconditionally** overwrites the phase to `Sweeping` without checking if `execute_final_mark` left remaining work.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires: incremental marking enabled with low thresholds
fn test_incremental_fallback_worklist_abandon() {
    // 1. Allocate many objects forming a reference chain: A -> B -> C -> D
    let a = Gc::new(Node { ref: Some(Gc::new(Node { ref: Some(...) })) });
    
    // 2. Force incremental mark by calling collect_full() first
    collect_full();
    
    // 3. Create OLD->YOUNG reference to trigger generational barrier
    // and dirty pages
    
    // 4. Trigger fallback by exhausting dirty_pages budget
    // (via many OLD->YOUNG writes)
    
    // 5. The bug: D is in state.worklist but never traced
    //    When sweep runs, D is reclaimed even though reachable via A->B->C->D
    
    // 6. Access through the chain causes UAF
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option 1 (Recommended):** Have `execute_final_mark` actually process `state.worklist`:

```rust
// After draining visitor.worklist to state.worklist, process it:
while let Some(ptr) = state.pop_work() {
    unsafe {
        trace_and_mark_object(ptr, state);
    }
    total_marked += 1;
}
```

**Option 2:** Have `collect_major_incremental` respect `execute_final_mark`'s phase decision and not unconditionally sweep.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The incremental marking spec requires that all marked objects be fully traced before sweep begins. The `execute_final_mark` function was intended to be a "final sweep" that processes remaining work, but it only drains SATB buffers without actually tracing the remaining worklist items. This is a fundamental violation of the incremental marking invariant that reachable objects must be preserved during sweep.

**Rustacean (Soundness 觀點):**
This is a soundness bug - reachable objects being reclaimed is undefined behavior in safe Rust terms (use-after-free). The `state.set_phase(MarkPhase::Sweeping)` at line 1845 unconditionally overwrites any phase set by `execute_final_mark`, creating a TOCTOU where the remaining worklist is silently abandoned.

**Geohot (Exploit 觀點):**
This is a reliable exploit primitive - by controlling dirty page pressure, an attacker can reliably trigger the fallback path and cause reachable objects to be swept. The race is deterministic given dirty page threshold. This could be used to UAF any GC-managed object by creating a reference chain and triggering fallback before the chain is fully traced.