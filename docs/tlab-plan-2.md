# Multi-threaded GC Handshake Plan

This document outlines the design for implementing a multi-threaded Garbage Collection Handshake in `rudo-gc`, drawing inspiration from the **Chez Scheme** implementation.

## 1. Reference: Chez Scheme Handshake Mechanism

Based on the analysis of Chez Scheme's C and Scheme source code, its handshake mechanism (specifically in `pthreads` mode) follows these principles:

- **Cooperative Rendezvous**: Threads are not stopped via asynchronous signals (like `SIGUSR1`). Instead, they check a "something pending" flag at predefined **Safe Points**.
- **Active Thread Tracking**: A global counter (`active_threads`) tracks how many threads are currently executing Scheme code or holding pointers that the GC must see.
- **Deactivation/Reactivation**: Threads entering blocking C calls (I/O, Mutex wait, FFI) call `deactivate_thread()`, decrementing the active counter. This allows GC to proceed without waiting for these threads to reach a safe point.
- **Handshake Protocol**:
    1. A thread triggers GC and sets a global `collect_request_pending` flag.
    2. All mutator threads eventually see this flag at a Safe Point and enter a **Rendezvous**.
    3. Threads wait on a `Condition Variable` if they are not the "collector".
    4. The **last active thread** (active counter == 1) becomes the collector and performs the GC.
    5. After GC, it broadcasts to the Condition Variable to resume all mutators.

---

## 2. Proposed Architecture for `rudo-gc`

### 2.1 Thread Registry & Control Block

We need a way to track all threads that own a `LocalHeap`.

```rust
// Global Registry in GlobalSegmentManager
pub struct GlobalSegmentManager {
    // ... existing fields ...
    /// All active thread control blocks.
    threads: Vec<Arc<ThreadControlBlock>>,
}

/// Shared control block for each thread.
pub struct ThreadControlBlock {
    /// Atomic state of the thread.
    pub state: AtomicUsize, // Executing, AtSafePoint, Inactive
    /// Flag set by the collector to request a handshake.
    pub gc_requested: AtomicBool,
    /// Condition variable to park the thread during GC.
    pub park_cond: Condvar,
    pub park_mutex: Mutex<()>,
    /// Pointer to the thread's LocalHeap (for sweeping).
    /// SAFETY: Only accessible by the collector when thread is parked or inactive.
    pub heap_ptr: *mut LocalHeap,
}

const THREAD_STATE_EXECUTING: usize = 0;
const THREAD_STATE_SAFEPOINT: usize = 1;
const THREAD_STATE_INACTIVE: usize = 2;
```

### 2.2 Safe Point Implementation

Safe points are locations where a thread is guaranteed to be in a "clean" state (no registers holding untracked pointers to GC memory, or all roots spilled to stack).

- **Automatic Check**: Insert a check in `LocalHeap::alloc` and `alloc_slow`.
- **Manual Check**: Provide a `rudo_gc::safepoint()` function for long-running non-allocating loops.
- **Poll Logic**:
  ```rust
  #[inline(always)]
  fn check_safepoint() {
      if GC_REQUESTED.load(Ordering::Relaxed) {
          enter_rendezvous();
      }
  }
  ```

### 2.3 Cooperative Rendezvous Protocol

1. **GC Trigger**: A thread (Thread A) notices `collect_condition` is met.
2. **Global Lock**: Thread A acquires the `GlobalSegmentManager` lock.
3. **Request**: Thread A sets `gc_requested = true` in all registered `ThreadControlBlock`s.
4. **Wait (STW)**: Thread A waits until `active_threads` (threads in `EXECUTING` state) drops to 1.
   - Threads in `INACTIVE` (blocked in syscalls) or `SAFEPOINT` are considered "stopped".
5. **Collection**: Thread A iterates through all `LocalHeap`s in the registry:
   - Calls `S_close_off_thread_local_segment` (flushes TLABs).
   - Performs Mark-Sweep across all pages.
6. **Resume**: Thread A sets `gc_requested = false` and signals all `park_cond` condition variables.

---

## 3. Implementation Steps

### Phase 1: Registry & Lifecycle
- Modify `LocalHeap` to register itself with `GlobalSegmentManager` upon creation.
- Implement a `ThreadControlBlock` that lives as long as the thread.
- Ensure `LocalHeap::drop` unregisters the thread.

### Phase 2: Safe Point Integration
- Replace `RefCell<LocalHeap>` with a more flexible storage (e.g., a wrapper that manages the `ThreadControlBlock`).
- Add `check_safepoint()` calls to `alloc` fast-path (carefully, for performance).

### Phase 3: Coordination Logic
- Implement the `park()` and `unpark()` logic using `Condvar`.
- Update `gc::collect()` to handle the multi-threaded coordination instead of just local collection.

---

## 4. Challenges & Mitigations

| Challenge | Mitigation |
| :--- | :--- |
| **Fast Path Overhead** | Use a single `Relaxed` atomic load. On modern CPUs, this is very cheap. |
| **Deadlocks** | Strictly define lock ordering (Registry Lock -> TC Mutex). |
| **Non-Allocating Threads** | Rely on manual `safepoint()` or eventually implement signal-based suspension for extreme cases. |
| **Pointer Stability** | Ensure `LocalHeap` address is stable (e.g., `Box`ed) so the Registry can hold a raw pointer safely. |

## 5. Summary

By adopting Chez Scheme's **Cooperative Rendezvous**, `rudo-gc` can support multi-threaded applications while maintaining the performance benefits of TLABs and the safety of Rust. The transition from `RefCell` to a thread-coordinated `UnsafeCell` model is the most critical structural change required.
