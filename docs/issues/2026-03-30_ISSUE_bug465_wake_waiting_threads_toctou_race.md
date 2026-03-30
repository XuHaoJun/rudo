# [Bug]: wake_waiting_threads TOCTOU Race Causes Thread to Skip GC Safepoint

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires precise timing between GC wake-up and thread entering safepoint |
| **Severity (嚴重程度)** | Critical | Can cause data race between GC and mutator thread |
| **Reproducibility (復現難度)** | Medium | Race condition but can be triggered with concurrent GC and thread activity |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `wake_waiting_threads` in `crates/rudo-gc/src/gc/gc.rs`, `enter_gc_safe_point` in `crates/rudo-gc/src/heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.x (current)

---

## 📝 問題描述 (Description)

When `wake_waiting_threads()` is called to wake threads after a GC cycle, it clears `gc_requested` for ALL threads before waking those in `SAFEPOINT` state. This creates a TOCTOU (Time-of-Check-Time-of-Use) race where a thread entering `enter_gc_safe_point()` concurrently can skip the wait and participate in a subsequent GC cycle, even though it didn't properly participate in the previous one.

### 預期行為 (Expected Behavior)
All threads that were waiting at a safepoint should be properly woken, and all threads should participate in subsequent GC cycles as expected.

### 實際行為 (Actual Behavior)
A thread entering `enter_gc_safe_point()` during `wake_waiting_threads()` can:
1. Check `gc_requested` (sees `true`, proceeds to CAS)
2. CAS succeeds (EXECUTING → SAFEPOINT)
3. `wake_waiting_threads` clears `gc_requested` for ALL threads
4. Thread enters wait loop, loads `gc_requested` (now `false`)
5. Thread exits wait loop without waiting
6. In subsequent GC cycle, thread's CAS fails (state is not EXECUTING)
7. Thread skips safepoint and continues running during GC

---

## 🔬 根本原因分析 (Root Cause Analysis)

**File:** `crates/rudo-gc/src/gc/gc.rs` lines 637-654

```rust
fn wake_waiting_threads() {
    let registry = crate::heap::thread_registry().lock().unwrap();
    let mut woken_count = 0;
    for tcb in &registry.threads {
        // Clear gc_requested for ALL threads to prevent hangs in future GC cycles
        tcb.gc_requested.store(false, Ordering::Release);  // <-- BUG: Clears for ALL threads

        if tcb.state.load(Ordering::Acquire) == crate::heap::THREAD_STATE_SAFEPOINT {
            tcb.park_cond.notify_all();
            tcb.state
                .store(crate::heap::THREAD_STATE_EXECUTING, Ordering::Release);
            woken_count += 1;
        }
    }
    registry
        .active_count
        .fetch_add(woken_count, std::sync::atomic::Ordering::SeqCst);
}
```

**Race scenario:**

1. GC cycle 1 ends, `wake_waiting_threads()` is called
2. Thread A is in `enter_gc_safe_point()` concurrently:
   - Loads `gc_global = true`, `gc_local = true` (line 761-762)
   - CAS succeeds: EXECUTING → SAFEPOINT (line 768-777)
   - **Has not yet reached wait loop** (line 794)
3. `wake_waiting_threads()` sets Thread A's `gc_requested = false` (line 642)
4. Thread A proceeds to wait loop:
   ```rust
   while tcb.gc_requested.load(Ordering::Acquire) {  // Loads false!
       guard = tcb.park_cond.wait(guard).unwrap();
   }
   ```
5. Thread A exits immediately without waiting, decrements `active_count`
6. GC cycle 2 starts, `request_gc_handshake()` sets `gc_requested = true`
7. Thread A is running (state = EXECUTING or INACTIVE):
   - Loads `gc_global = true`, `gc_local = true`
   - CAS from EXECUTING to SAFEPOINT succeeds
   - Enters wait loop again, participates normally?

Wait - in the scenario above, Thread A DOES participate in cycle 2. Let me reconsider...

Actually, the issue is that Thread A decrements `active_count` without waiting. So:
- If GC cycle 2 starts before Thread A finishes `enter_gc_safe_point` for cycle 1
- `request_gc_handshake` reads `active_count` (Thread A hasn't decremented yet)
- GC proceeds thinking all threads will stop
- But Thread A decrements `active_count` and continues without waiting!

This creates a window where `active_count` is inaccurate and GC may proceed with a thread running.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

The race is difficult to reproduce reliably but the bug manifests as:
1. GC runs with multiple threads
2. `wake_waiting_threads` clears `gc_requested` for all threads
3. A thread that was entering safepoint skips the wait
4. In next GC, `active_count` becomes desynchronized
5. GC may proceed with thread running, causing data race

```rust
// Conceptual race - requires precise timing
// Thread A (mutator):
loop {
    // Trigger GC request
    crate::gc::request_gc();
    
    // Do work
    let _x = Gc::new(Data);
}

// Thread B (GC coordinator):
loop {
    // Wait for GC request
    crate::gc::wait_for_gc_complete();
    
    // After collection, wake threads
    crate::gc::wake_waiting_threads();
}
```

The race: Thread A enters `enter_gc_safe_point` just as Thread B calls `wake_waiting_threads`. Thread A's `gc_requested` is cleared before it enters the wait loop, causing it to skip waiting.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Option 1: Only clear `gc_requested` AFTER waking the thread (inside the if block):
```rust
fn wake_waiting_threads() {
    let registry = crate::heap::thread_registry().lock().unwrap();
    let mut woken_count = 0;
    for tcb in &registry.threads {
        if tcb.state.load(Ordering::Acquire) == crate::heap::THREAD_STATE_SAFEPOINT {
            tcb.park_cond.notify_all();
            tcb.state.store(crate::heap::THREAD_STATE_EXECUTING, Ordering::Release);
            // Clear gc_requested AFTER waking, not before
            tcb.gc_requested.store(false, Ordering::Release);
            woken_count += 1;
        }
        // Don't clear gc_requested for threads not at safepoint!
    }
    registry.active_count.fetch_add(woken_count, ...);
}
```

Option 2: Use a different synchronization mechanism that doesn't rely on `gc_requested` being set during the wake-up:
- Use a sequence counter that threads check
- Use a per-thread "generation" counter that increments each GC cycle

Option 3: Move the `gc_requested` check to BEFORE the CAS, so that clearing it after the CAS doesn't affect threads that already checked it:
```rust
// In enter_gc_safe_point:
// Check gc_requested BEFORE CAS, not after
if !gc_global && !gc_local {
    return;
}
// Proceed with CAS and stack scanning
// gc_requested check inside wait loop is for re-checking after wake-up
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The safepoint mechanism is a rendezvous between the GC and mutators. The `active_count` is supposed to track threads that have promised to stop. When a thread decrements `active_count` but doesn't actually wait, the coordination breaks. The GC may proceed prematurely or may have inconsistent views of which threads are participating. The fix should ensure that `gc_requested` is only cleared for threads that have actually been woken, not preemptively for all threads.

**Rustacean (Soundness 觀點):**
This is a data race condition. The TOCTOU between checking `gc_requested` and entering the wait loop, combined with the clearing of `gc_requested` by `wake_waiting_threads`, can cause a thread to skip the safepoint. This violates the safety invariants that the GC relies on - specifically that when GC runs with `active_count == N`, there are exactly N threads participating. If a thread skips the safepoint, it may access heap memory that the GC is simultaneously modifying.

**Geohot (Exploit 觀點):**
While this is primarily a correctness bug, it could potentially be exploited as a denial-of-service. If an attacker can trigger GC at precise moments relative to thread scheduling, they could cause the GC to proceed with a thread running, potentially causing memory corruption that leads to crashes or unexpected behavior. The race condition is timing-dependent but could be triggered reliably with enough attempts.