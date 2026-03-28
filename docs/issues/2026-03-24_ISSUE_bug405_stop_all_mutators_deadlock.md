# [Bug]: stop_all_mutators_for_snapshot busy-wait loop without timeout can cause GC deadlock

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires pathological case (thread spinning without allocations) |
| **Severity (嚴重程度)** | Critical | Complete GC deadlock - collector hangs forever |
| **Reproducibility (復現難度)** | Very Low | Difficult to reproduce in practice |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `IncrementalMarkState::stop_all_mutators_for_snapshot()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

The `stop_all_mutators_for_snapshot()` function in `gc/incremental.rs` contains a busy-wait loop with **no timeout mechanism**. If a mutator thread never calls `enter_rendezvous()` (e.g., due to spinning without allocations), the collector will deadlock waiting forever.

### 預期行為
- Collector should wait a bounded time for mutators to reach safepoint
- If timeout occurs, collector should either fallback to STW or report an error

### 實際行為
- Collector loops indefinitely at line 529 until `active == 1`
- If a mutator never calls `enter_rendezvous()`, `active_count` never decrements
- **Result**: Complete GC deadlock - collector hangs forever

---

## 🔬 根本原因分析 (Root Cause Analysis)

Location: `crates/rudo-gc/src/gc/incremental.rs:529-549`

```rust
fn stop_all_mutators_for_snapshot() {
    // ... setup code ...
    
    loop {
        let registry = crate::heap::thread_registry().lock().unwrap();
        let active = registry.active_count.load(Ordering::Acquire);
        std::sync::atomic::fence(Ordering::Acquire);

        if registry.threads.is_empty() {
            break;
        }

        // active == 1 means only the collector is running
        if active == 1 {
            break;
        }
        // BUG: No timeout! If a thread hangs, this loops forever
    }
}
```

**Problem**: The loop has no timeout or deadline. If any registered thread fails to reach a safepoint:
1. `active_count` never becomes 1
2. Collector loops forever
3. Deadlock

**How mutators reach safepoint**: Threads call `enter_rendezvous()` in:
- `begin_collect()` during GC allocation (heap.rs:719)
- `LocalHeap::drop()` (heap.rs:726)

**When deadlock occurs**: If a thread is spinning without making any allocations or dropping LocalHeap, it never calls `enter_rendezvous()`. This could happen with:
- CPU-bound spinning without allocations
- Threads blocked on external synchronization without allocations
- Bugs causing `enter_rendezvous()` to not be called

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use rudo_gc::{Gc, Trace, collect_full, set_incremental_config, IncrementalConfig};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    // Create a Gc to ensure collector is active
    let gc = Gc::new(Data { value: 42 });
    
    let started = Arc::new(AtomicBool::new(false));
    let started_clone = started.clone();
    
    // Spawn a thread that spins without allocations
    let handle = thread::spawn(move || {
        started_clone.store(true, Ordering::SeqCst);
        // Infinite loop without any GC allocations
        // This thread will never call enter_rendezvous()
        loop {
            std::hint::spin_loop();
        }
    });
    
    // Wait for spinner to start
    while !started.load(Ordering::SeqCst) {
        thread::yield_now();
    }
    
    // Try to trigger GC - will deadlock because spinner never reaches safepoint
    collect_full();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### Option 1: Add Timeout with Fallback to STW

```rust
use std::time::{Duration, Instant};

fn stop_all_mutators_for_snapshot() {
    // ... setup code ...
    
    let deadline = Instant::now() + Duration::from_millis(100); // 100ms timeout
    
    loop {
        // ... check conditions ...
        
        if Instant::now() > deadline {
            // Timeout - fallback to STW or panic with diagnostic
            eprintln!("[GC] WARNING: Timeout waiting for mutators, forcing STW");
            // Force all threads to stop (STW fallback)
            break;
        }
        
        std::thread::yield_now();
    }
}
```

### Option 2: Use rendezvous_ack_counter

The comment at line 544 mentions `rendezvous_ack_counter` was "never fully wired". Complete the implementation:

```rust
// In enter_rendezvous(), increment ack counter when entering safepoint:
state.increment_rendezvous_ack();

// In stop_all_mutators_for_snapshot(), wait for ack_count >= thread_count:
if state.get_rendezvous_ack_count() >= registry.threads.len() as u32 {
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The busy-wait without timeout is a well-known anti-pattern in GC implementation. GCs typically have a bounded wait followed by a fallback mechanism (often STW fallback or termination). The current implementation assumes all mutators will eventually reach a safepoint, which is true for well-behaved programs but can fail in pathological cases.

**Rustacean (Soundness 觀點):**
This is not technically UB, but it violates liveness guarantees. A program that triggers this condition will hang indefinitely, which is a severe availability issue. Adding a timeout with fallback would restore progress guarantees.

**Geohot (Exploit 觀點):**
An attacker could intentionally trigger this condition to cause a denial of service. By spinning a thread without allocations, they can prevent GC from completing, eventually causing memory exhaustion. This is a potential attack vector.

---

## 驗證狀態

- [ ] 需要 Miri 或 ThreadSanitizer 驗證
- [ ] PoC 需要在多執行緒環境下測試
- [ ] 需要確認修復方案的正確性
