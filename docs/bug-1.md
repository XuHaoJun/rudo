# Deep Dive Analysis: `crates/rudo-gc`

## Executive Summary

As requested, I have performed a deep dive into the `rudo-gc` crate, focusing on critical bugs, concurrency safety, and garbage collection correctness. The analysis has uncovered **two critical bugs** that compromise the stability and safety of the system: a race condition in the parallel marker and a logic error in the sweeper that corrupts the free list.

## Critical Findings

### 1. Data Race in Parallel Marker Overflow Queue
**Severity**: Critical
**Location**: `src/gc/marker.rs`, `clear_overflow_queue` (lines 159-179) and `push_overflow_work` (lines 83-117).

**Description**:
The synchronization mechanism between `push_overflow_work` (readers) and `clear_overflow_queue` (writer) is flawed. The `clear_overflow_queue` function attempts to acquire exclusive access by checking `users == 0` (line 162) *before* acquiring the generation lock (CASing `clear_gen`, line 165). This check-then-act sequence is not atomic.

**Scenario**:
1. **Clearer**: Reads `users` as 0.
2. **Pusher**: Calls `fetch_add` (users becomes 1).
3. **Pusher**: Reads `clear_gen` (sees Even/Safe).
4. **Pusher**: Proceeds to access queue.
5. **Clearer**: CAS `clear_gen` to Odd (Lock Acquired). The clearer assumes it has exclusive access because of step 1.
6. **Result**: The Pusher and Clearer concurrently access/modify the queue. Since the Clearer destroys nodes (`Box::from_raw`), the Pusher may experience use-after-free or corrupt the linked list state.

**Impact**:
Heap corruption, Segfaults, and undefined behavior during parallel marking phases.

**Recommendation**:
Invert the locking logic in `clear_overflow_queue`. It must signal intent (set generation) *before* waiting for readers to drain.

```rust
pub fn clear_overflow_queue() {
    // 1. Acquire Lock (Signal intent)
    let old_gen = OVERFLOW_QUEUE_CLEAR_GEN.fetch_add(1, Ordering::AcqRel);
    
    // 2. Wait for existing users to drain
    loop {
        let users = OVERFLOW_QUEUE_USERS.load(Ordering::Acquire);
        if users == 0 {
            break;
        }
        std::hint::spin_loop();
    }
    
    // 3. Clear queue
    // ...
}
```

### 2. Infinite Loop / Corrupted Free List in Sweeper
**Severity**: Critical
**Location**: `src/gc/gc.rs`, `sweep_phase2_reclaim` (lines 1248-1325).

**Description**:
The sweep logic iterates through objects to rebuild the free list. When a garbage object is encountered (`is_alloc && !is_marked`):
1. It is reclaimed, and added to the local `free_head` (lines 1283-1290). `free_head` is updated to point to this index `i`.
2. `is_alloc` is set to `false`.
3. The code falls through to the next `if !is_alloc` block (line 1298) in the *same iteration*.
4. It attempts to add the same index `i` to the free list *again*.
5. It writes `free_head` (which is `i`) into the slot `i`.
6. This creates a self-referential cycle: `slot[i] -> i`.

The `head_written` check (line 1304) fails to prevent this because it checks against `(*header).free_list_head`, which holds the *old* head (from the start of the sweep or unrelated) and is not updated until the loop finishes.

**Impact**:
The free list becomes corrupted with a self-cycle. The next time `Tlab::alloc` tries to allocate from this page, it will enter an infinite loop or repeatedly return the same address, leading to massive memory corruption.

**Recommendation**:
Prevent fall-through when an object is reclaimed.

```rust
if is_alloc && !is_marked {
    // ... reclamation logic ...
    reclaimed += 1;
    // Do NOT set is_alloc = false here to avoid entering the next block,
    // OR use 'continue', OR use 'else if'.
    continue; 
}

if !is_alloc {
    // ... free list logic ...
}
```

## Additional Observations

### `check_safepoint` in `Tlab::alloc`
The `check_safepoint` logic (checking `GC_REQUESTED`) is generally correct for cooperative suspension. However, ensure that `dec_ref` (which can run arbitrary destructors) does not inadvertently cause issues if a safepoint is requested during a `drop`. Currently, `dec_ref` does not check for safepoints, which is acceptable as long as `drop` chains are bounded or `safepoint` is checked in loops.

### Conservative Stack Scanning
The `scan_heap_region_conservatively` function uses `HEAP` thread-local storage. This is correct because scanning is performed by the thread itself (via `enter_rendezvous` -> `spill_registers_and_scan`).

---
**R. Kent Dybvig**
*Professor of Computer Science*
