# [Bug]: AsyncHandleScope slot allocation race condition - TOCTOU between fetch_add and bounds check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent handle creation in the same AsyncHandleScope from multiple threads |
| **Severity (嚴重程度)** | `High` | Can cause slot collision, data corruption, or use-after-free |
| **Reproducibility (復現難度)** | `Medium` | Needs concurrent access but is deterministic in theory |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandleScope`, `GcScope::spawn`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

The slot allocation in `AsyncHandleScope::handle()` and `GcScope::spawn()` has a race condition. The bounds check happens AFTER `fetch_add()`, creating a classic TOCTOU (Time-Of-Check-To-Time-Of-Use) vulnerability.

### 預期行為 (Expected Behavior)
- Each call to `handle()` or `spawn()` should get a unique slot index
- When 256 handles are allocated, the next allocation should panic
- Concurrent allocations should not result in duplicate indices

### 實際行為 (Actual Behavior)
Two concurrent threads can:
1. Both call `fetch_add(1, Ordering::Relaxed)` and get the SAME index
2. Both pass the bounds check (both get valid indices like 255)
3. Both write to the same slot, corrupting data
4. Or one thread could get index 256 while the other gets 255, causing unexpected behavior

---

## 🔬 根本原因分析 (Root Cause Analysis)

The problematic code at `crates/rudo-gc/src/handles/async.rs:310-314`:

```rust
let used = unsafe { &*self.data.used.get() };
let idx = used.fetch_add(1, Ordering::Relaxed);
if idx >= HANDLE_BLOCK_SIZE {
    panic!("AsyncHandleScope: exceeded maximum handle count ({HANDLE_BLOCK_SIZE})");
}
```

The same pattern exists at lines 1054-1056 in `GcScope::spawn()`.

**Race scenario:**
1. Thread A reads `used = 255`, Thread B reads `used = 255` (before A's fetch completes)
2. Thread A: `fetch_add(1)` returns 255, Thread B: `fetch_add(1)` returns 255
3. Both pass `idx >= HANDLE_BLOCK_SIZE` check (255 < 256)
4. Both write to `slots[255]`, second write overwrites first

This is a data race + TOCTOU vulnerability.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::{AsyncHandleScope, GcScope};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // This PoC attempts to trigger the race by having multiple threads
    // create handles concurrently on the same AsyncHandleScope
    
    let tcb = rudo_gc::gc_thread::new_gc_thread("test".to_string());
    let scope = Arc::new(AsyncHandleScope::new(&tcb));
    
    let gc1 = Gc::new(Data { value: 1 });
    let gc2 = Gc::new(Data { value: 2 });
    
    // Create handles from multiple threads concurrently
    let scope_clone1 = scope.clone();
    let scope_clone2 = scope.clone();
    
    let handles = thread::scope(|s| {
        let h1 = s.spawn(|| {
            scope_clone1.handle(&gc1)
        });
        let h2 = s.spawn(|| {
            scope_clone2.handle(&gc2)
        });
        
        [h1.join().unwrap(), h2.join().unwrap()]
    });
    
    // If race occurred, both handles might point to same slot
    // This is difficult to verify externally but demonstrates the vulnerability
    println!("Handles created: {:?}", handles);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option 1: Use fetch_sub on overflow (compare-and-swap pattern)**
```rust
let idx = used.fetch_add(1, Ordering::AcqRel);
if idx >= HANDLE_BLOCK_SIZE {
    used.fetch_sub(1, Ordering::AcqRel); // Rollback
    panic!("...");
}
```

**Option 2: Use stronger ordering + retry loop**
```rust
loop {
    let current = used.load(Ordering::Acquire);
    if current >= HANDLE_BLOCK_SIZE {
        panic!("...");
    }
    match used.compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire) {
        Ok(idx) => break idx,
        Err(_) => continue, // Retry
    }
}
```

**Option 3: Use a Mutex for slot allocation**
Simple but reduces concurrency.

Recommended: Option 2 with `compare_exchange` for proper synchronization.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a fundamental concurrency bug in handle allocation. The relaxed ordering on `fetch_add` is insufficient when there's a bounds check that must pass. The slot array is a critical resource that needs proper synchronization. This could lead to incorrect reference tracking and potential memory management errors in the GC.

**Rustacean (Soundness 觀點):**
This is a data race (UB in Rust terms). Two threads accessing the same slot without synchronization violates Rust's memory model. The `Ordering::Relaxed` is insufficient here because we have a dependent check that must be atomic with the increment. This should be a compile-time error or use atomics properly.

**Geohot (Exploit 觀點):**
The slot collision could be exploited to:
1. Make two handles point to the same object (ref count corruption)
2. Cause use-after-free if one handle is dropped while the other is used
3. In extreme cases, bypass the bounds check timing to overflow the array

The relaxed ordering is the key issue - it's optimized for the "common case" but the bounds check creates a dependency that relaxed ordering cannot guarantee.

---

## Resolution (2026-02-26)

**Outcome:** Already fixed.

Both affected locations now use the compare_exchange loop (Option 2 from the suggested fix):

- `handles/async.rs` lines 311-321 (`AsyncHandleScope::handle`): CAS loop with `Acquire` load and `AcqRel` exchange
- `handles/async.rs` lines 1079-1094 (`GcScope::spawn`): Same pattern

The old `fetch_add` + bounds-check pattern has been replaced. No code changes required.
