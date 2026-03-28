# [Bug]: stop_all_mutators_for_snapshot timeout fallback cleared by execute_snapshot reset_fallback

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Timeout only occurs when mutators spin without allocations |
| **Severity (嚴重程度)** | High | Fallback mechanism never triggers - GC proceeds incrementally with mutators running |
| **Reproducibility (復現難度)** | Medium | Can be triggered with CPU-bound spinning thread |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `execute_snapshot()` in `gc/incremental.rs:545-604`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `stop_all_mutators_for_snapshot()` times out (line 496), it calls `request_fallback()` to set `fallback_requested = true`. The `execute_snapshot` function should preserve this flag so that subsequent calls to `mark_slice()` can detect the fallback request and trigger STW behavior.

### 實際行為 (Actual Behavior)
At line 551 in `execute_snapshot()`, `state.reset_fallback()` is called **unconditionally**, immediately clearing the `fallback_requested` flag that was just set by the timeout handling:

```rust
pub fn execute_snapshot(heaps: &[&LocalHeap]) -> usize {
    stop_all_mutators_for_snapshot();  // If timeout: sets fallback_requested = true

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    state.stats().reset();
    state.reset_fallback();  // BUG: Clears fallback_requested immediately!
    // ... proceeds to do snapshot with mutators potentially running ...
}
```

The `mark_slice()` function checks `fallback_requested()` at lines 622 and 638 to return `MarkSliceResult::Fallback`, but by then the flag has been cleared.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is at `gc/incremental.rs:551`:

```rust
state.reset_fallback();  // <-- BUG: Always clears fallback_requested
```

When timeout occurs in `stop_all_mutators_for_snapshot()`:
1. Line 501: `state.request_fallback(FallbackReason::SliceTimeout)` sets `fallback_requested = true`
2. Line 502: `break` exits the loop
3. `stop_all_mutators_for_snapshot()` returns
4. Line 551: `state.reset_fallback()` clears the flag

Later in `mark_slice()`:
```rust
if state.fallback_requested() {  // <-- Always false! Flag was cleared!
    let reason = state.stats().fallback_reason();
    return MarkSliceResult::Fallback { reason };
}
```

The fallback mechanism is completely bypassed.

---

## 💣 重現步驟 / PoC (Proof of Concept)

```rust
// Trigger conditions:
// 1. Set incremental config with short slice timeout
// 2. Spawn thread that spins without allocations
// 3. Trigger GC
// 4. stop_all_mutators_for_snapshot times out after 100ms
// 5. fallback_requested is set to true
// 6. execute_snapshot calls reset_fallback() - flag cleared!
// 7. mark_slice never sees fallback_requested = true
// 8. GC proceeds incrementally with mutator still spinning (incorrect)
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move `reset_fallback()` to BEFORE the call to `stop_all_mutators_for_snapshot()`, so that only intentional fallback requests from timeout handling are preserved:

```rust
pub fn execute_snapshot(heaps: &[&LocalHeap]) -> usize {
    let state = IncrementalMarkState::global();
    state.reset_fallback();  // <-- Move here to reset at start of cycle
    
    stop_all_mutators_for_snapshot();  // May set fallback_requested = true via request_fallback()
    // fallback_requested is now preserved if timeout occurred
    
    state.set_phase(MarkPhase::Snapshot);
    state.stats().reset();
    // No more reset_fallback() here!
    // ...
}
```

Or alternatively, only reset fallback if it wasn't requested:
```rust
pub fn execute_snapshot(heaps: &[&LocalHeap]) -> usize {
    stop_all_mutators_for_snapshot();
    
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    state.stats().reset();
    if !state.fallback_requested() {
        state.reset_fallback();  // Only reset if not already requested
    }
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The fallback mechanism is designed to switch from incremental to STW when certain conditions occur (timeout, dirty pages exceeded, etc.). The timeout in `stop_all_mutators_for_snapshot` is supposed to trigger this fallback. But since `reset_fallback()` clears the flag immediately, the fallback never happens. This defeats the entire purpose of the timeout mechanism.

**Rustacean (Soundness 觀點):**
When timeout occurs, the snapshot may be taken with mutators still running (not at safepoint). This violates the STW invariant that `execute_snapshot` is supposed to maintain. If a mutator has a pointer in a register, that pointer won't be scanned as a root, potentially causing premature collection.

**Geohot (Exploit 觀點):**
An attacker could trigger this condition by spinning a thread without allocations. The timeout would occur, but the GC would continue incrementally instead of falling back to STW. This could be exploited to cause memory leaks or inconsistent GC state.
