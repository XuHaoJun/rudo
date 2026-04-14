# [Bug]: execute_snapshot continues incremental path after timeout triggers fallback

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Timeout only occurs when mutators spin without allocations for 100ms+ |
| **Severity (嚴重程度)** | High | STW invariant violated; mutators run with write barrier disabled during incremental marking |
| **Reproducibility (Reproducibility)** | Medium | Can be triggered with CPU-bound spinning thread during GC |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `execute_snapshot()` in `gc/incremental.rs:545-604`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `stop_all_mutators_for_snapshot()` times out (line 496-502), it should cause the incremental marking to abort and fall back to STW. The `execute_snapshot` function should check if `fallback_requested` is true after the stop attempt and NOT proceed with incremental marking.

### 實際行為 (Actual Behavior)
After timeout fires in `stop_all_mutators_for_snapshot()`:
1. `fallback_requested = true` is set (line 501)
2. Control breaks from the loop
3. `execute_snapshot` continues at line 551: `state.set_phase(MarkPhase::Snapshot)`
4. Later sets phase to `Marking` (line 597)
5. `debug_assert!(write_barrier_needed())` at line 598 **panics** in debug builds because `write_barrier_needed()` returns `false` when `fallback_requested = true`
6. In release builds: `resume_all_mutators()` releases mutators with write barrier disabled
7. `mark_slice()` called later sees `fallback_requested = true` and returns `Fallback`

The incremental marking protocol continues even though a fallback was triggered.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is the **missing check** for `fallback_requested` after `stop_all_mutators_for_snapshot()` returns.

```rust
pub fn execute_snapshot(heaps: &[&LocalHeap]) -> usize {
    let state = IncrementalMarkState::global();
    state.reset_fallback();

    stop_all_mutators_for_snapshot();  // May set fallback_requested = true if timeout

    // BUG: No check for fallback_requested here!
    
    state.set_phase(MarkPhase::Snapshot);  // Continues anyway...
    // ...
    state.set_phase(MarkPhase::Marking);
    
    debug_assert!(
        write_barrier_needed(),  // PANIC: write_barrier_needed() returns false when fallback_requested = true
        "Write barrier must be active before resuming mutators"
    );
    resume_all_mutators();
    state.start_slice();
    count
}
```

When timeout occurs in `stop_all_mutators_for_snapshot()`:
1. Line 501: `state.request_fallback(FallbackReason::SliceTimeout)` sets `fallback_requested = true`
2. Line 502: `break` exits the loop
3. `execute_snapshot` continues without checking `fallback_requested`

The function `write_barrier_needed()` returns:
```rust
state.is_enabled() && !state.fallback_requested() && is_write_barrier_active()
```
When `fallback_requested = true`, this returns `false`, causing the `debug_assert!` to panic.

---

## 💣 重現步驟 / PoC (Proof of Concept)

```rust
// Trigger conditions:
// 1. Spawn thread that spins without allocations (never reaches safepoint)
// 2. Trigger incremental GC
// 3. stop_all_mutators_for_snapshot times out after 100ms
// 4. fallback_requested is set to true
// 5. execute_snapshot continues - sets phases and resumes mutators
// 6. debug_assert!(write_barrier_needed()) PANICS in debug builds
//    OR in release: mutators run with write barrier disabled

#[test]
fn test_timeout_triggers_fallback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;
    
    // Create a spinning thread that never allocates
    let started = AtomicBool::new(false);
    let handle = thread::spawn(move || {
        started.store(true, Ordering::SeqCst);
        // Spin forever without allocating
        loop {
            std::hint::spin_loop();
        }
    });
    
    // Wait for thread to start
    while !started.load(Ordering::SeqCst) {
        thread::yield_now();
    }
    
    // Trigger GC - should timeout waiting for spinning thread
    collect_full();
    
    handle.join().unwrap(); // Never reached
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add a check after `stop_all_mutators_for_snapshot()` to return early if fallback was requested:

```rust
pub fn execute_snapshot(heaps: &[&LocalHeap]) -> usize {
    let state = IncrementalMarkState::global();
    state.reset_fallback();

    stop_all_mutators_for_snapshot();

    // ADD THIS CHECK:
    if state.fallback_requested() {
        // Timeout occurred - cannot safely do incremental marking
        // The mutators are still running and the snapshot is invalid
        // Let mark_slice() handle the fallback properly
        return 0;
    }

    state.set_phase(MarkPhase::Snapshot);
    // ... rest of function
}
```

Alternatively, perform proper STW fallback:
```rust
if state.fallback_requested() {
    // Need to actually stop all mutators for STW
    // This requires re-implementing the stop logic or using a different approach
    panic!("STW fallback not implemented for timeout case");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The timeout mechanism in `stop_all_mutators_for_snapshot` is a safety net for when mutators can't reach safepoint. But after setting `fallback_requested = true`, the code continues with the incremental path as if nothing happened. This defeats the purpose of the timeout fallback. The snapshot may be taken with mutators still modifying the heap, violating the STW invariant that `execute_snapshot` is supposed to maintain.

**Rustacean (Soundness 觀點):**
The `debug_assert!(write_barrier_needed())` at line 598 will panic in debug builds when `fallback_requested = true`. In release builds, the code proceeds with mutators resumed and write barriers disabled - this is undefined behavior waiting to happen. The incremental marking algorithm assumes write barriers are active, so proceeding without them can cause premature collection of live objects.

**Geohot (Exploit 觀點):**
An attacker could trigger this condition by spinning a thread without allocations during GC. The timeout would occur, but instead of proper STW fallback, the GC continues incrementally with the write barrier disabled. This could be exploited to cause memory leaks or inconsistent GC state. The `debug_assert` panic could also be leveraged for denial of service.

---

## 驗證 Pattern 符合性

- **Pattern 1 (Full GC 遮蔽 barrier bug)**: 不適用 - 此 bug 在 timeout/fallback 情境
- **Pattern 2 (單執行緒無法觸發競態)**: 需多執行緒 (一個 spinning thread + GC thread)
- **Pattern 3 (測試情境不符)**: 此為增量標記 timeout 問題，與其他 bug 不同
- **Pattern 4 (容器內的 Gc 未被當作 root)**: 不適用
- **Pattern 5 (難以觀察的內部狀態)**: debug_assert panic 可觀察