# [Bug]: push_overflow_work returns Err without retrying after clearing completes

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Can occur during normal GC when clearing coincides with overflow push |
| **Severity (嚴重程度)** | `Medium` | Work items can be silently lost during GC |
| **Reproducibility (復現難度)** | `High` | Concurrent timing required, but can be constructed with threads |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/marker.rs` - `push_overflow_work` function
- **OS / Architecture:** `All` - concurrent GC operations
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

In `gc/marker.rs`, the `push_overflow_work` function has a race condition when clearing of the overflow queue coincides with a push operation.

### 預期行為 (Expected Behavior)
When a pusher detects that clearing is in progress, it should either:
1. Wait for clearing to finish and then retry the push, OR
2. Return error but ensure no work is silently lost

### 實際行為 (Actual Behavior)
The function decrements its user count, waits for clearing to finish, then returns `Err(work)` without attempting to push. This causes work items to be silently lost.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `push_overflow_work` (lines 83-117):

```rust
fn push_overflow_work(work: *const GcBox<()>) -> Result<(), *const GcBox<()>> {
    OVERFLOW_QUEUE_USERS.fetch_add(1, Ordering::AcqRel);  // Line 84
    loop {
        let clear_gen = OVERFLOW_QUEUE_CLEAR_GEN.load(Ordering::Acquire);
        if clear_gen % 2 == 1 {  // Line 87: Clearing in progress
            OVERFLOW_QUEUE_USERS.fetch_sub(1, Ordering::AcqRel);  // Line 88
            loop {
                std::hint::spin_loop();
                let new_gen = OVERFLOW_QUEUE_CLEAR_GEN.load(Ordering::Acquire);
                if new_gen % 2 == 0 {  // Line 92: Clearing done
                    break;
                }
            }
            return Err(work);  // Line 96: Returns WITHOUT retrying push!
        }
        // ... CAS push attempt ...
    }
}
```

**The race:**
1. Pusher increments USERS at line 84
2. Pusher sees clear_gen is odd (clearing in progress) at line 87
3. Pusher decrements USERS at line 88 (it will wait for clearing to finish)
4. `clear_overflow_queue` spins waiting for USERS == 0, proceeds when true
5. `clear_overflow_queue` drains the queue at lines 202-206
6. `clear_overflow_queue` increments clear_gen to even at line 207
7. Pusher exits its spin loop at line 93
8. Pusher returns `Err(work)` at line 96 - **work item is lost!**

The pusher returns error because it detected clearing was in progress, but by the time it exits the spin loop, clearing has finished AND the queue has been drained. The pusher should retry the push, not return an error.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudo-PoC - requires careful thread synchronization
fn poc_lost_work() {
    // Set up a thread that will push to overflow queue
    let pusher = thread::spawn(|| {
        // This work item should be pushed
        push_overflow_work(work_item);  
    });

    // Concurrently, trigger clear_overflow_queue
    // such that it happens while pusher is in the spin loop
    
    pusher.join();
    // The work_item is LOST - clear_overflow_queue drained it
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

The fix is simple: after the spin loop exits (clearing is done), the pusher should **retry the push** instead of returning an error. Remove the early `return Err(work)` at line 96, or restructure the loop:

Option 1: Remove the inner loop and return, let outer loop retry:
```rust
if clear_gen % 2 == 1 {
    OVERFLOW_QUEUE_USERS.fetch_sub(1, Ordering::AcqRel);
    // Wait for clearing to finish
    loop {
        std::hint::spin_loop();
        let new_gen = OVERFLOW_QUEUE_CLEAR_GEN.load(Ordering::Acquire);
        if new_gen % 2 == 0 {
            break;
        }
    }
    // Fall through to retry instead of return Err
}
```

Option 2: After spin loop, simply continue to retry the push:
```rust
if clear_gen % 2 == 1 {
    OVERFLOW_QUEUE_USERS.fetch_sub(1, Ordering::AcqRel);
    loop {
        std::hint::spin_loop();
        let new_gen = OVERFLOW_QUEUE_CLEAR_GEN.load(Ordering::Acquire);
        if new_gen % 2 == 0 {
            break;
        }
    }
    OVERFLOW_QUEUE_USERS.fetch_add(1, Ordering::AcqRel);  // Re-add ourselves
    continue;  // Retry the push
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic synchronization bug where a worker gives up too early. In Chez Scheme's GC, we use handshakes that ensure workers don't exit until the phase is complete. The pusher should retry until the push succeeds or a bounded timeout expires.

**Rustacean (Soundness 觀點):**
This is a logical race condition, not a memory safety issue. The work item is owned by the caller and they may handle the error, but the bug causes work to be silently lost when the error path is taken incorrectly.

**Geohot (Exploit 觀點):**
An attacker could engineer this race to cause GC work items to be lost, potentially causing references to be collected prematurely if the lost work represented GC roots or heap references.
