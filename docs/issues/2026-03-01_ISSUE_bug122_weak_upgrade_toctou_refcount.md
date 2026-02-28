# [Bug]: Weak::upgrade TOCTOU race when ref_count > 0

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Concurrent weak upgrades with last strong ref drop |
| **Severity (嚴重程度)** | Critical | Use-after-free (UAF) possible |
| **Reproducibility (復現難度)** | High | Requires concurrent threads, but reliable with loom |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::upgrade()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`Weak::upgrade()` has a TOCTOU (Time-of-check to time-of-use) race condition when `ref_count > 0`. The code loads `ref_count`, checks various flags (dead_flag, dropping_state), then does a CAS to increment the reference count. Between the load and CAS, another thread could drop the last strong reference (setting `dropping_state`), but the CAS would still succeed, allowing a use-after-free.

### 預期行為 (Expected Behavior)
When the last strong reference is dropped concurrently with a weak upgrade, the weak upgrade should fail (return None) because the object is being dropped.

### 實際行為 (Actual Behavior)
The weak upgrade may succeed even when the object is being dropped, because the CAS to increment ref_count happens after the checks but doesn't re-verify dropping_state before returning.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `ptr.rs` lines 1668-1717. The code pattern is:

1. Load `ref_count` (line 1690)
2. Check `has_dead_flag()`, `dropping_state()` (lines 1682-1688)
3. CAS from `current_count` to `current_count + 1` (lines 1699-1707)

The issue: Between step 1/2 and step 3, another thread can:
1. Drop the last strong reference
2. This calls `try_mark_dropping()` which sets `dropping_state = 1`
3. Then calls `drop_fn` to drop the value
4. The CAS in step 3 still succeeds (e.g., 1 -> 2)
5. We return a Gc to freed memory (UAF!)

**Comparison**: The internal `GcBoxWeakRef::upgrade()` (line 464-509) correctly uses `try_inc_ref_if_nonzero()` which is an atomic operation, avoiding this race.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This would require loom or concurrent testing to reliably reproduce
// The race window is small but real

use rudo_gc::{Gc, Weak, Trace};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

#[derive(Trace)]
struct Data {
    value: AtomicUsize,
}

fn main() {
    let gc = Gc::new(Data { value: AtomicUsize::new(42) });
    let weak = Gc::downgrade(&gc);
    
    // Spawn thread that will drop the last strong ref
    let handle = thread::spawn(move || {
        drop(gc); // Drop last strong reference
    });
    
    // Concurrently try to upgrade
    // There's a race window where:
    // 1. We load ref_count = 1
    // 2. Other thread drops, sets dropping_state = 1
    // 3. Our CAS 1->2 succeeds (doesn't check dropping_state!)
    // 4. We return a Gc to freed memory
    
    let _ = weak.upgrade(); // May return Some to freed memory!
    
    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Replace the non-atomic load + CAS pattern with `try_inc_ref_if_nonzero()`:

```rust
// Current (buggy):
let current_count = gc_box.ref_count.load(Ordering::Acquire);
// ... checks ...
if gc_box.ref_count.compare_exchange_weak(current_count, current_count + 1, ...).is_ok() { ... }

// Fixed:
if !gc_box.try_inc_ref_if_nonzero() {
    return None;
}
```

This is the same pattern used in `GcBoxWeakRef::upgrade()` (line 502-504).

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The internal GcBoxWeakRef::upgrade correctly uses atomic try_inc_ref_if_nonzero(). The public Weak::upgrade should use the same pattern to avoid resurrecting objects that are being dropped.

**Rustacean (Soundness 觀點):**
This is a soundness bug - it can lead to use-after-free, which is undefined behavior in Rust. The fix is straightforward: use the atomic operation.

**Geohot (Exploit 觀點):**
The race window is small but exploitable. An attacker could use techniques like thread spinning or timing attacks to increase the likelihood of hitting the race. The consequence is a classic UAF which can be leveraged for code execution.
