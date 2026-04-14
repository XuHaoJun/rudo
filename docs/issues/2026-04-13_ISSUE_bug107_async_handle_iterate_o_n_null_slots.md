# [Bug]: AsyncHandleScope::iterate() performs O(N) scan with null slots

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | HandleScope churn is common in long-running async apps |
| **Severity (嚴重程度)** | `Medium` | Performance degredation, potential stale pointer visit |
| **Reproducibility (復現難度)** | `Medium` | Requires many handle create/drop cycles |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandleScope::iterate()`, `crates/rudo-gc/src/handles/async.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

`AsyncHandleScope::iterate()` uses `used` counter to bound iteration, but `used` only increments via `compare_exchange` when new handles are allocated. When handles are dropped, `used` is NOT decremented, leading to:
1. O(N) iteration where N = total handles ever allocated, not active handles
2. Visiting null slots and potentially stale pointers

### 預期行為 (Expected Behavior)
Iteration should only visit active (non-null) handles, O(K) where K = active handle count.

### 實際行為 (Actual Behavior)
Iteration scans all `used` slots, many of which are null after handle drop. O(N) where N = cumulative handles allocated.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `crates/rudo-gc/src/handles/async.rs:432-443`:
```rust
pub fn iterate<F>(&self, mut visitor: F)
where
    F: FnMut(*const GcBox<()>),
{
    let used = unsafe { &*self.data.used.get() }.load(Ordering::Acquire);
    let slots = unsafe { &*self.data.block.slots.get() };
    for slot in slots.iter().take(used) {  // used is NOT decremented on drop!
        if !slot.is_null() {
            visitor(slot.as_ptr());
        }
    }
}
```

The `used` atomic counter is only incremented in `handle()` via `compare_exchange`, but `Drop` of `AsyncHandleGuard` sets the slot to null without decrementing `used`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;

fn main() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = rudo_gc::handles::async::AsyncHandleScope::new(&tcb);
    
    // Allocate and immediately drop 255 handles
    for i in 0..255 {
        let gc = Gc::new(i);
        let handle = scope.handle(&gc);
        drop(handle);
        drop(gc);
    }
    
    // allocate ONE real handle
    let gc = Gc::new(999);
    let handle = scope.handle(&gc);
    
    // iterate() will scan 255 null slots + 1 real slot
    let mut count = 0;
    scope.iterate(|ptr| {
        count += 1;
    });
    
    println!("Visited {} slots (expected 1, got 256)", count);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Option 1: Add decrement logic to `used` when handle slot is freed
Option 2: Use a free list for slot management instead of `used` counter
Option 3: Track active handle count separately from `used`

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The iterate() method is called during GC marking to visit all handles as roots. Scanning null slots wastes GC time but doesn't affect correctness since null slots are skipped. However, if a slot is reused before GC visits it, stale pointer could be visited.

**Rustacean (Soundness 觀點):**
No soundness issue directly - null slots are checked before visitor callback. But visiting 255 null slots per GC cycle is inefficient.

**Geohot (Exploit 觀點):**
If slot reuse happens aggressively, iterate() might visit a stale pointer from a reused slot that was not yet re-traced by GC. Low probability but theoretically possible.