# [Bug]: sweep_segment_pages called unconditionally while phase is Marking (UAF risk)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Only triggers when incremental marking falls back to execute_final_mark with remaining > 0 |
| **Severity (嚴重程度)** | `Catastrophic` | Use-after-free can lead to arbitrary code execution |
| **Reproducibility (復現難度)** | `High` | Need specific incremental marking workload with dirty pages threshold hit |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking, `collect_major_incremental`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`sweep_segment_pages` and `sweep_large_objects` should only be called when `state.phase() == MarkPhase::Sweeping`. Objects still being marked should not be reclaimed.

### 實際行為 (Actual Behavior)
Sweep functions are called unconditionally at lines 1853-1854 in `collect_major_incremental`, even when `remaining > 0` causes phase to remain `Marking`. This can cause objects that are still being traced to be prematurely reclaimed, leading to use-after-free.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `collect_major_incremental` (crates/rudo-gc/src/gc/gc.rs:1840-1854):

1. When `remaining > 0 || dirty_pages > 0`, `execute_final_mark` is called
2. `execute_final_mark` may set phase to `Marking` if there's remaining work (to continue marking in next cycle)
3. Comment at line 1844-1847 confirms: "execute_final_mark sets the phase based on remaining work. If remaining > 0, it sets phase to Marking"
4. **BUG**: Lines 1853-1854 unconditionally call `sweep_segment_pages` and `sweep_large_objects` without checking if phase is `Sweeping`

Historical context:
- Commit `9ecee2f` fixed this by wrapping sweep calls in `if state.phase() == MarkPhase::Sweeping`
- Commit `6fffe2e` ("Fix bug490: memory leak when incremental marking fallback has remaining work") reverted this fix

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable incremental marking with `set_incremental_config()`
2. Create workload that triggers incremental marking with dirty pages threshold hit
3. When `remaining > 0` after `execute_final_mark`, sweep will run while phase is still `Marking`
4. Objects still in the mark worklist may be incorrectly swept

```rust
// Pseudocode PoC
set_incremental_config(IncrementalConfig { dirty_pages_threshold: 10, .. });
let gc = Gc::new(data);
let other = Gc::new(other_data);
// Create references that trigger incremental marking
// Force dirty_pages > threshold to trigger execute_final_mark with remaining > 0
collect_full(); // or minor GC that triggers incremental path
// If sweep runs while Marking phase, referenced objects may be UAF'd
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Restore the phase check before calling sweep functions:

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

The memory leak (bug490) that motivated the revert should be addressed by allowing incremental marking to complete rather than forcing unconditional sweep.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The incremental marking algorithm requires strict phase ordering: Marking → Sweeping. When `execute_final_mark` leaves work un-done (remaining > 0), it intentionally keeps phase at Marking so marking can continue. Calling sweep during this transition violates the state machine invariants and can reclaim objects that are live but not yet marked.

**Rustacean (Soundness 觀點):**
This is a soundness bug. Objects accessible through GC roots must not be reclaimed while they are still traceable. The unconditional sweep breaks the GC's safety invariants and can lead to UB when accessing a reclaimed object.

**Geohot (Exploit 觀點):**
Use-after-free in GC-managed memory is a classic exploit primitive. An attacker who can control the timing of incremental marking fallback could potentially arrange for a controlled object to be freed while a reference remains, enabling memory corruption attacks.