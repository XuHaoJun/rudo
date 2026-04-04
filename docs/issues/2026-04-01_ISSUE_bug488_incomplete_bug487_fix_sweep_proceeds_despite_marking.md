# [Bug]: Incomplete bug487 fix - sweep proceeds despite Marking phase

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | When incremental marking has remaining work after final mark, this bug triggers |
| **Severity (嚴重程度)** | Catastrophic | USE-AFTER-FREE of reachable objects |
| **Reproducibility (復現難度)** | High | Requires specific incremental marking workload to trigger remaining > 0 after final mark |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Major GC (gc/gc.rs `incremental_collect_with_heap`)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

The "fix" for bug487 at line 1853-1857 is incomplete. The code detects when `state.phase() != MarkPhase::Sweeping` (meaning `execute_final_mark` determined there is still marking work to do), but **still proceeds to sweep** at lines 1860-1861.

### 預期行為 (Expected Behavior)
When `execute_final_mark` sets phase to `Marking` (because remaining work exists), the sweep should be **skipped** or a fallback should be triggered. Reachability must be fully established before reclaiming any objects.

### 實際行為 (Actual Behavior)
The check at line 1853 detects the problem and logs a comment:
```rust
// This shouldn't happen if execute_final_mark works correctly.
// Remaining work exists but we're about to sweep - this is a bug.
// For now, we'll proceed but this may cause USE-AFTER-FREE.
```
But then lines 1860-1861 **proceed anyway**:
```rust
let reclaimed = sweep_segment_pages(heap, false);
let reclaimed_large = sweep_large_objects(heap, false);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `collect_major_incremental` (gc.rs:1807-1867):

1. Line 1841: `if remaining > 0 || dirty_pages > 0`
2. Line 1843: `execute_final_mark(heaps_mut)` is called
3. Line 1844-1847: Comment explains `execute_final_mark` sets phase based on remaining work
4. Line 1853-1854: **BUG**: `sweep_segment_pages` and `sweep_large_objects` are called UNCONDITIONALLY
5. Line 1855: Only `promote_all_pages` is gated on `state.phase() == MarkPhase::Sweeping`

The bug487 fix removed the unconditional `set_phase(Sweeping)` at the end, but the sweep itself is still unconditional. When `execute_final_mark` sets phase to `Marking` (because remaining work exists), the sweep STILL proceeds at lines 1853-1854, violating the incremental GC invariant.

**Current code (gc.rs:1852-1858):**
```rust
timer.start();
let reclaimed = sweep_segment_pages(heap, false);  // UNCONDITIONAL!
let reclaimed_large = sweep_large_objects(heap, false);  // UNCONDITIONAL!
if state.phase() == MarkPhase::Sweeping {
    promote_all_pages(heap);  // Only this is gated
}
timer.end_sweep();
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This bug requires specific conditions where `execute_final_mark` finds remaining work but the caller doesn't properly handle it:

1. Start an incremental major GC
2. Create a workload where some objects are only reachable through a chain that requires multiple marking passes
3. Trigger `incremental_collect_with_heap` with `remaining > 0` after `execute_final_mark`
4. The phase will be set to `Marking` but sweep will still occur

```rust
// Pseudocode for trigger condition
// This occurs when work remains after execute_final_mark
// but the function incorrectly proceeds to sweep anyway
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

The fix should return early or trigger fallback when phase is not Sweeping:

```rust
// FIX bug487 (properly this time): Only sweep if phase is Sweeping.
// If phase is Marking, execute_final_mark determined there's still work to do.
// We must NOT sweep in this case - return early and let the next cycle handle it.
if state.phase() != MarkPhase::Sweeping {
    // Request fallback to STW or return early
    // Sweeping unmarked (but reachable) objects here causes USE-AFTER-FREE
    state.set_phase(MarkPhase::Idle);
    return CollectResult {
        objects_reclaimed: 0,
        timer,
        collection_type: crate::metrics::CollectionType::IncrementalMajor,
    };
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The bug is a clear violation of the incremental GC invariant: never reclaim an object that may still be reachable. When `execute_final_mark` returns with `remaining > 0`, it signals that the mark phase is incomplete. Proceeding to sweep in this state violates the fundamental safety of mark-sweep GC. The phase state machine should be treated as authoritative.

**Rustacean (Soundness 觀點):**
This is a memory safety violation. The code acknowledges the issue in a comment but doesn't prevent the unsound behavior. The `// SAFETY` contract for sweep is that all unmarked objects are unreachable - but this invariant is violated when sweep runs while the mark phase is still active. This is undefined behavior territory.

**Geohot (Exploit 觀點):**
USE-AFTER-FREE vulnerabilities are exploitable when an attacker can control the timing of allocation and object lifetimes. In a GC system, if an object is prematurely swept while references still exist (through dangling pointers), an attacker who can trigger GC at specific times could achieve arbitrary read/write primitives through object reallocation.

---

## Resolution (2026-04-04)

**Outcome:** Fixed.

Applied fix to `crates/rudo-gc/src/gc/gc.rs` in `collect_major_incremental()`:

```rust
timer.start();
if state.phase() != MarkPhase::Sweeping {
    state.set_phase(MarkPhase::Idle);
    return CollectResult {
        objects_reclaimed: 0,
        timer,
        collection_type: crate::metrics::CollectionType::IncrementalMajor,
    };
}
let reclaimed = sweep_segment_pages(heap, false);
let reclaimed_large = sweep_large_objects(heap, false);
promote_all_pages(heap);
timer.end_sweep();
```

Verification:
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- `./test.sh` passes
