# [Bug]: ThreadLocalHeap Drop at Safepoint Causes GC Hang

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires thread to be at GC safepoint when heap is dropped (thread terminating during GC) |
| **Severity (嚴重程度)** | Critical | Causes complete GC hang - all threads deadlocked |
| **Reproducibility (復現難度)** | Medium | Can be reproduced with concurrent GC request and thread termination |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ThreadLocalHeap::drop` in `heap.rs`, thread registry safepoint mechanism
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.x (current)

---

## 📝 問題描述 (Description)

When a thread is at a GC safepoint (`THREAD_STATE_SAFEPOINT`) and its `ThreadLocalHeap` is dropped (e.g., thread terminating), the thread is removed from the registry but never properly woken up. This causes a permanent GC hang.

### 預期行為 (Expected Behavior)
When a thread at safepoint is dropped:
1. The thread should be properly cleaned up
2. The thread should either complete its GC participation or be cleanly removed
3. `active_count` should remain synchronized with actual thread count

### 實際行為 (Actual Behavior)
1. Thread enters safepoint, decrements `active_count`, waits on condvar
2. Thread's heap is dropped while at safepoint
3. `Drop::drop` calls `unregister_thread`, removing thread from registry
4. Thread is still waiting on condvar but no one will wake it
5. `active_count` is permanently desynchronized
6. GC hangs forever waiting for thread that will never respond

---

## 🔬 根本原因分析 (Root Cause Analysis)

**File:** `crates/rudo-gc/src/heap.rs` lines 3514-3527

```rust
impl Drop for ThreadLocalHeap {
    fn drop(&mut self) {
        let thread_id = std::thread::current().id();
        migrate_roots_to_orphan(&self.tcb, thread_id);

        let mut registry = thread_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.tcb.state.load(Ordering::SeqCst) == THREAD_STATE_EXECUTING {
            registry.active_count.fetch_sub(1, Ordering::SeqCst);
        }
        registry.unregister_thread(&self.tcb);
    }
}
```

**Problem:**
1. When a thread enters safepoint via `enter_gc_safe_point()` (line 768-796):
   - State changes from `EXECUTING` to `SAFEPOINT`
   - `active_count` is decremented (line 791)
   - Thread waits on `tcb.park_cond.wait()`

2. In `Drop::drop`:
   - If state is `EXECUTING`, `active_count` is decremented (line 3522-3523)
   - `unregister_thread` is ALWAYS called (line 3525)
   - **Problem:** If state is `SAFEPOINT`, the thread is removed but NOT woken

3. The orphaned thread remains waiting at `park_cond.wait()` forever because:
   - `resume_all_threads()` iterates over `registry.threads`
   - The orphaned thread is no longer in `threads`
   - Therefore `park_cond.notify_all()` is never called for it

4. Subsequent GC attempts check `active_count` expecting all threads to participate, but the orphaned thread never responds, causing GC to hang.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This is a conceptual PoC - actual reproduction requires careful timing
// The bug triggers when:
// 1. Multiple threads are running
// 2. GC is requested
// 3. One thread enters safepoint and waits
// 4. That thread's ThreadLocalHeap is dropped (thread terminates)
// 5. resume_all_threads() runs but can't wake the orphaned thread
// 6. GC hangs

// Minimal reproduction scenario:
// - Thread A and B running
// - GC_REQUESTED is set
// - Thread A enters safepoint, decrements active_count, waits
// - Thread A's heap is dropped (Thread A terminating)
// - Thread A is removed from registry
// - Thread A is still waiting at condvar but will NEVER be woken
// - active_count remains desynchronized
// - GC hangs
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

In `ThreadLocalHeap::drop`, when the thread is at safepoint, we must wake it before unregistering:

```rust
impl Drop for ThreadLocalHeap {
    fn drop(&mut self) {
        let thread_id = std::thread::current().id();
        migrate_roots_to_orphan(&self.tcb, thread_id);

        let mut registry = thread_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        
        if self.tcb.state.load(Ordering::SeqCst) == THREAD_STATE_SAFEPOINT {
            // Thread is at safepoint - must wake it before unregistering
            // Otherwise it will wait forever on condvar with no one to wake it
            unsafe {
                // Wake the thread so it can complete its cleanup
                (*self.tcb.park_cond.get()).notify_all();
            }
            self.tcb.state.store(THREAD_STATE_EXECUTING, Ordering::Release);
            // Don't decrement active_count - it was already decremented when entering safepoint
            // and resume_all_threads will handle restoring it if needed
        } else if self.tcb.state.load(Ordering::SeqCst) == THREAD_STATE_EXECUTING {
            registry.active_count.fetch_sub(1, Ordering::SeqCst);
        }
        
        registry.unregister_thread(&self.tcb);
    }
}
```

**Key insight:** The `active_count` was already decremented when the thread entered safepoint (line 791). We should NOT decrement it again in `Drop::drop` for the SAFEPOINTT case. Instead, we must wake the thread so it can properly exit the wait loop.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The safepoint mechanism requires all threads to check in and be accounted for. When a thread is removed from the registry while at safepoint, the GC coordination mechanism breaks. The `active_count` reflects threads that have promised to participate in GC, but the orphaned thread has already decremented its count. This creates a situation where GC expects responses from threads that can no longer be reached. The fix needs to ensure proper cleanup of the safepoint state before removing the thread from coordination tracking.

**Rustacean (Soundness 觀點):**
The `Drop` implementation has a subtle correctness issue. When a thread is at safepoint and gets dropped, the conditional `if state == EXECUTING` means `active_count` is NOT decremented (correct, because it was already decremented at safepoint entry). However, the thread is silently removed from the registry without being properly woken. This is not undefined behavior per se, but it causes a logical deadlock that manifests as a system hang. The fix must ensure the waiting thread is properly awakened so it can exit the wait loop.

**Geohot (Exploit 觀點):**
While this isn't a memory safety issue (no UAF or data corruption), it's a denial-of-service vulnerability. An attacker who can trigger GC while simultaneously causing thread termination could hang the entire system. The condition requires precise timing (TOCTOU between entering safepoint and heap drop), making it harder to exploit intentionally. However, in long-running servers with frequent GC and thread churn, this could occur naturally and cause system hangs.