# [Bug]: GcHandle::resolve panic during cross-thread handle migration (bug313)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires precise timing between thread termination and handle resolution |
| **Severity (嚴重程度)** | High | Causes panic (denial of service) when resolving cross-thread handle |
| **Reproducibility (復現難度)** | Medium | Needs thread termination + handle resolution timing alignment |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve` / `migrate_roots_to_orphan`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.x (012-cross-thread-gchandle feature)

---

## 📝 問題描述 (Description)

When a thread terminates while cross-thread GC handles still exist for objects it created, `migrate_roots_to_orphan()` is called to transfer the handles from the terminating thread's TCB to the global orphan table. During this migration, there exists a race window where `GcHandle::resolve()` may panic with "handle has been unregistered" even though the handle is valid and in the process of being migrated.

### 預期行為 (Expected Behavior)

`GcHandle::resolve()` should always find the handle either in the TCB roots (if migration hasn't started) or in the orphan table (if migration has completed). The handle should never be in neither location.

### 實際行為 (Actual Behavior)

`resolve()` may panic with "GcHandle::resolve: handle has been unregistered" during the migration window.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The race occurs due to the lock acquisition order in `migrate_roots_to_orphan()` vs `resolve()`:

**`migrate_roots_to_orphan()` (heap.rs:176-196):**
1. Acquire TCB roots lock
2. Drain entries to local vector
3. **Release TCB roots lock** ← Entry is now in neither TCB nor orphan!
4. Acquire orphan lock
5. Insert entries into orphan
6. Release orphan lock

**`resolve()` (cross_thread.rs:186-206):**
1. Upgrade TCB reference
2. Acquire TCB roots lock
3. Check TCB roots - if empty, drop lock
4. Acquire orphan lock
5. Check orphan table

**The Race Window:**
After `migrate_roots_to_orphan` releases the TCB roots lock (step 3) but before it acquires the orphan lock (step 4), the entry exists in the local `drained` vector, not in either data structure. If `resolve()` runs during this window:
1. It acquires TCB roots lock, sees empty, releases
2. It acquires orphan lock before migration has inserted the entry
3. It finds the entry missing and panics

Code reference: `cross_thread.rs:199-203` explicitly comments on this: "Migration window (bug313): roots drained, entry only in orphan table."

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
//! PoC for bug313: GcHandle::resolve panic during migration
//! Run with: cargo test --test cross_thread_handle -- --test-threads=1

use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_resolve_during_migration_race() {
    let handle = Arc::new(Gc::cross_thread_handle(Data { value: 42 }));
    let resolve_count = Arc::new(AtomicUsize::new(0));
    let start_resolving = Arc::new(AtomicBool::new(false));
    let migration_complete = Arc::new(AtomicBool::new(false));
    
    // Spawn a thread that will constantly try to resolve the handle
    let handle_clone = handle.clone();
    let resolve_count_clone = resolve_count.clone();
    let start_clone = start_resolving.clone();
    let migration_complete_clone = migration_complete.clone();
    
    let resolver_thread = thread::spawn(move || {
        // Wait for signal to start resolving
        while !start_clone.load(std::sync::atomic::Ordering::Relaxed) {
            thread::yield_now();
        }
        
        // Try to resolve repeatedly until migration completes
        while !migration_complete_clone.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(gc) = handle_clone.try_resolve() {
                assert_eq!(gc.value, 42);
                resolve_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            thread::yield_now();
        }
    });
    
    // Give the resolver thread time to start
    thread::sleep(Duration::from_micros(100));
    start_resolving.store(true, std::sync::atomic::Ordering::Relaxed);
    
    // Spawn multiple threads that create and drop handles rapidly
    // This increases chance of triggering the race
    let handles: Vec<_> = (0..10).map(|_| {
        let h = Gc::cross_thread_handle(Data { value: 100 });
        thread::spawn(move || {
            drop(h);
        })
    }).collect();
    
    // Wait for all threads to terminate (triggering migration)
    for t in handles {
        t.join().unwrap();
    }
    
    // Signal migration is complete
    migration_complete.store(true, std::sync::atomic::Ordering::Relaxed);
    resolver_thread.join().unwrap();
    
    // If we get here without panic, the race wasn't triggered this time
    // In practice, this test may need to run many iterations
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option A: Insert into orphan BEFORE draining from TCB roots**
Modify `migrate_roots_to_orphan()` to:
1. Acquire orphan lock first
2. Insert entries into orphan
3. Then drain from TCB roots

But this requires changing the check order in `resolve()` to check orphan first.

**Option B: Hold both locks simultaneously**
Use `Mutex` to protect a compound operation, or use a single lock for both data structures.

**Option C: Retry logic in resolve()**
If the handle is not found in orphan, re-check TCB roots and retry the orphan lookup before panicking.

**Recommended Fix:**
Option C is simplest to implement without major refactoring. Add a small retry loop in `resolve()` when TCB is alive but entry not found in TCB roots.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The orphan migration is a necessary operation when threads terminate. The GC must maintain roots for handles that outlive their creating thread. The current implementation correctly ensures handles are tracked, but the lock ordering creates a window where a handle is "in transit" and invisible to `resolve()`. This is an implementation detail of the migration mechanism, not a fundamental GC design flaw.

**Rustacean (Soundness 觀點):**
This is not a memory safety issue (no UAF or use-after-free), but it IS a correctness issue causing panic. The panic is recoverable, but code that expects `resolve()` to always succeed (when handle is valid) may not handle this case. The bug demonstrates that "handle is valid" and "resolve succeeds" are not equivalent during the narrow migration window.

**Geohot (Exploit 觀點):**
While this is a denial-of-service (panic) rather than a memory corruption bug, it could be exploited in scenarios where:
1. An attacker can trigger thread termination at will
2. The application has untrusted code that creates cross-thread handles
3. The panic propagates to crash a service

However, since this is a library bug (not application logic), exploitation is unlikely in practice.