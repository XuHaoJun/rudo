# [Bug]: Large Object Allocation Missing ref_count/weak_count Initialization

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Large object allocation happens when objects exceed page size |
| **Severity (嚴重程度)** | High | Uninitialized atomic fields can cause incorrect reference counting and use-after-free |
| **Reproducibility (複現難度)** | Medium | Needs memory reuse scenario or specific timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `alloc_large` / heap.rs:2417-2426
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

When allocating a large object via `alloc_large`, the function only initializes `drop_fn` and `trace_fn`, but does NOT initialize `ref_count`, `weak_count`, or `is_dropping` atomic fields. This leaves these fields with potentially garbage values from uninitialized memory.

### 預期行為 (Expected Behavior)
All `GcBox` fields should be properly initialized:
- `ref_count`: Should be 1 for a newly allocated object
- `weak_count`: Should be 0 (no weak refs yet)
- `is_dropping`: Should be 0 (not being dropped)

### 實際行為 (Actual Behavior)
In `alloc_large` (heap.rs:2421-2425):
```rust
let gc_box_ptr = ptr.as_ptr().add(h_size).cast::<crate::ptr::GcBox<()>>();
std::ptr::addr_of_mut!((*gc_box_ptr).drop_fn)
    .write(crate::ptr::GcBox::<()>::no_op_drop);
std::ptr::addr_of_mut!((*gc_box_ptr).trace_fn)
    .write(crate::ptr::GcBox::<()>::no_op_trace);
// MISSING: ref_count, weak_count, is_dropping initialization!
```

Compare with the normal `Gc::new` path (ptr.rs:867-874):
```rust
gc_box.write(GcBox {
    ref_count: AtomicUsize::new(1),
    weak_count: AtomicUsize::new(0),
    drop_fn: GcBox::<T>::drop_fn_for,
    trace_fn: GcBox::<T>::trace_fn_for,
    is_dropping: AtomicUsize::new(0),
    value,
});
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

When memory is reused (either from a deallocated large object or from the system), the atomic fields `ref_count`, `weak_count`, and `is_dropping` may contain garbage or stale values:

1. **ref_count with garbage**: If non-zero but wrong, reference counting becomes inconsistent. If accidentally zero, object could be prematurely collected.
2. **weak_count with stale flags**: Could contain `GEN_OLD_FLAG` or `DEAD_FLAG` bits that cause incorrect barrier behavior.
3. **is_dropping with garbage**: Could cause incorrect `weak::upgrade` race condition handling.

This is different from bug171 (slot allocation) - this is specifically about large object allocation which uses a different code path (`alloc_large` vs `try_pop_from_page`).

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This requires accessing internal APIs or special test configuration
// to verify uninitialized atomic fields.

use rudo_gc::*;

fn main() {
    // Allocate a large object (> page size, typically > 8KB)
    // This triggers alloc_large path
    let large_obj = Gc::new(vec![0u8; 16384]); // Large enough for large object path
    
    // Force GC to reclaim it
    drop(large_obj);
    collect_full();
    
    // Allocate another large object at same memory (if reused)
    let new_large_obj = Gc::new(vec![0u8; 16384]);
    
    // Check ref_count/weak_count - they may have garbage values
    // This requires internal API access to verify
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

In `alloc_large` (heap.rs:2421-2426), add initialization for all atomic fields:

```rust
let gc_box_ptr = ptr.as_ptr().add(h_size).cast::<crate::ptr::GcBox<()>>();
std::ptr::addr_of_mut!((*gc_box_ptr).drop_fn)
    .write(crate::ptr::GcBox::<()>::no_op_drop);
std::ptr::addr_of_mut!((*gc_box_ptr).trace_fn)
    .write(crate::ptr::GcBox::<()>::no_op_trace);

// Initialize atomic fields to safe defaults
std::ptr::addr_of_mut!((*gc_box_ptr).ref_count)
    .write(std::sync::atomic::AtomicUsize::new(1));
std::ptr::addr_of_mut!((*gc_box_ptr).weak_count)
    .write(std::sync::atomic::AtomicUsize::new(0));
std::ptr::addr_of_mut!((*gc_box_ptr).is_dropping)
    .write(std::sync::atomic::AtomicUsize::new(0));
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- The ref_count field is critical for reference counting-based collection triggering
- Uninitialized weak_count could contain GEN_OLD_FLAG or other bits causing barrier issues
- This is a memory management oversight - all fields must be properly initialized

**Rustacean (Soundness 觀點):**
- This could lead to UB if garbage values in atomic fields cause incorrect behavior
- The ref_count.load() expects valid values; garbage could cause panics or incorrect results
- is_dropping is used in race condition handling - garbage values could cause incorrect upgrades

**Geohot (Exploit 觀點):**
- If attacker can control memory layout, they could set specific ref_count values
- Could potentially trigger premature collection or prevent collection
- The uninitialized memory could leak sensitive data if not zeroed
