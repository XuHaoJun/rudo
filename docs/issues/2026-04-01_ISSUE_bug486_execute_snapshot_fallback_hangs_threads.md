# [Bug]: Threads hang forever when fallback_requested in execute_snapshot

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any timeout in stop_all_mutators_for_snapshot triggers this |
| **Severity (嚴重程度)** | Critical | Threads hang forever - entire program deadlocks |
| **Reproducibility (Reproducibility)** | High | Deterministic when timeout occurs |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `execute_snapshot()` in `gc/incremental.rs:551-553`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `stop_all_mutators_for_snapshot()` times out and sets `fallback_requested = true`, the function should:
1. Resume all stopped mutator threads
2. Return early to let `mark_slice()` handle the fallback

### 實際行為 (Actual Behavior)
After commit d3dacaa (fix for bug485):
1. `stop_all_mutators_for_snapshot()` times out and sets `fallback_requested = true`
2. Threads are waiting at safepoints (`while tcb.gc_requested.load(...)` in heap.rs:797)
3. `execute_snapshot` checks `fallback_requested` and returns early (line 552)
4. **BUG**: `resume_all_mutators()` is NEVER called
5. Threads remain stuck at safepoints forever - program deadlock

---

## 🔬 根本原因分析 (Root Cause Analysis)

The fix for bug485 (d3dacaa) added an early return when `fallback_requested` is true:

```rust
pub fn execute_snapshot(heaps: &[&LocalHeap]) -> usize {
    let state = IncrementalMarkState::global();
    state.reset_fallback();

    stop_all_mutators_for_snapshot();  // Sets gc_requested=true, threads wait at safepoint

    if state.fallback_requested() {
        return 0;  // BUG: Never resumes the stopped threads!
    }
    // ...
    resume_all_mutators();  // Only called here (line 606)
    // ...
}
```

When timeout occurs in `stop_all_mutators_for_snapshot()` (incremental.rs:496-502):
1. Line 501: `state.request_fallback(...)` sets `fallback_requested = true`
2. Line 502: `break` exits the loop
3. But `gc_requested = true` is already set on all thread TCBs
4. Threads are now waiting at safepoints (heap.rs:797)
5. The early return at line 552 skips `resume_all_mutators()`
6. Threads hang forever waiting for `gc_requested` to become false

---

## 💣 重現步驟 / PoC (Proof of Concept)

```rust
// Trigger conditions:
// 1. Spawn thread that spins without allocations (never reaches safepoint)
// 2. Trigger incremental GC
// 3. stop_all_mutators_for_snapshot times out after 100ms
// 4. fallback_requested is set to true
// 5. execute_snapshot returns early
// 6. Mutator thread is stuck at safepoint forever

#[test]
fn test_threads_hang_on_fallback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;
    use std::sync::mpsc::channel;
    
    let (tx, rx) = channel();
    
    let handle = thread::spawn(move || {
        // Signal that thread started
        tx.send(()).unwrap();
        
        // Spin forever without allocating
        loop {
            std::hint::spin_loop();
        }
    });
    
    // Wait for thread to start
    rx.recv().unwrap();
    
    // Trigger GC - should timeout waiting for spinning thread
    let started_at = std::time::Instant::now();
    collect_full();  // This will hang forever!
    
    // Never reached
    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `resume_all_mutators()` call before the early return:

```rust
if state.fallback_requested() {
    // CRITICAL: Must resume mutators before returning, otherwise they hang forever
    resume_all_mutators();
    return 0;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The handshake protocol requires that every stop has a corresponding resume. When the timeout fires, we set `gc_requested = true` on all threads, causing them to enter the safepoint wait loop. The early return skips the resume, violating the handshake protocol and causing a deadlock.

**Rustacean (Soundness 觀點):**
This is a pure deadlock bug - no memory safety issue, but the program becomes unresponsive. The fix is straightforward: call `resume_all_mutators()` before returning.

**Geohot (Exploit 觀點):**
Any code that triggers a GC while a CPU-bound spinloop is running will deadlock the entire process. This is an easy denial-of-service vector.

---

## 驗證 Pattern 符合性

- **Pattern 1 (Full GC 遮蔽 barrier bug)**: 不適用
- **Pattern 2 (單執行緒無法觸發競態)**: 不適用 - 單執行緒可觸發此 bug
- **Pattern 3 (測試情境不符)**: 不適用
- **Pattern 4 (容器內的 Gc 未被當作 root)**: 不適用
- **Pattern 5 (難以觀察的內部狀態)**: 可觀察 - 執行緒 hang 住不動