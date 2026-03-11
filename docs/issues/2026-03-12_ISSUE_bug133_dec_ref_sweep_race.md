# [Bug]: GcBox::dec_ref called on potentially swept slot causing memory corruption

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires precise timing between inc_ref, GC sweep, and slot reuse |
| **Severity (嚴重程度)** | Critical | Can cause memory corruption and use-after-free |
| **Reproducibility (復現難度)** | Very High | Requires specific race condition timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** GcBox::dec_ref, cross_thread_handle, Gc::clone, handle resolution
- **OS / Architecture:** Linux x86_64, All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

### 預期行為
When `GcBox::dec_ref` is called, the GcBox should be valid and the slot should not have been swept/reused by another object.

### 實際行為
In certain race conditions, `dec_ref` is called on a slot that may have been swept and potentially reused for a new allocation. This can lead to:
1. Reading incorrect `weak_count_raw` from a new object's memory
2. Calling `drop_fn` on memory that belongs to a different object
3. Memory corruption and undefined behavior

### Root Cause
The pattern appears in multiple locations:
- `ptr.rs:1583` - Gc::cross_thread_handle
- `ptr.rs:1694` - Gc::clone  
- `heap.rs:262` - clone_orphan_root_with_inc_ref
- `cross_thread.rs:213` - GcHandle::resolve
- `cross_thread.rs:437` - GcHandle::clone

The race condition:
1. Thread A has last reference (ref_count = 1), drops it, ref_count = 0
2. Object becomes "dead" but sweep hasn't run yet (slot still marked allocated)
3. Thread B calls cross_thread_handle/clone on a stale Gc pointer
4. Thread B's assert passes (object appears alive)
5. Thread B calls inc_ref(), which can increment from 0 to 1 (saturating_add)
6. GC sweep runs between inc_ref and is_allocated check
7. Sweep clears is_allocated (object was dead - ref_count was 0 before inc_ref)
8. is_allocated check fails
9. dec_ref is called on a potentially reused slot - MEMORY CORRUPTION!

---

## 🔬 根本原因分析 (Technical Details)

The `inc_ref` function uses `saturating_add` which allows incrementing from 0 to 1:

```rust
pub fn inc_ref(&self) {
    self.ref_count
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            if count == usize::MAX {
                None // Stay at MAX
            } else {
                Some(count.saturating_add(1))
            }
        })
        .ok();
}
```

Combined with the TOCTOU between assert checks and is_allocated verification, this creates a window where:
1. Object appears alive at assert time (ref_count >= 1 or dead_flag not set)
2. Another thread drops last reference between assert and inc_ref
3. GC sweep runs and clears is_allocated
4. dec_ref is called on memory that may belong to a new object

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// Requires specific timing to trigger
// High-speed thread communication with GC timing control
// Stress test with many threads creating/dropping Gc handles concurrently
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

Remove the `dec_ref` call before panicking. When is_allocated is false after inc_ref, the slot may have been reused - calling dec_ref on invalid memory is dangerous. Just panic without the dec_ref call.

Locations to fix:
1. `ptr.rs:1583` - Change to just panic
2. `ptr.rs:1694` - Change to just panic
3. `heap.rs:262` - Change to just panic  
4. `cross_thread.rs:213` - Change to just panic
5. `cross_thread.rs:437` - Change to just panic

Example fix for ptr.rs:1582-1585:
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    // Don't call dec_ref - slot may be reused!
    panic!("Gc::cross_thread_handle: object slot was swept after inc_ref");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The SATB barrier ensures objects allocated during marking are live. However, this race condition bypasses that protection by allowing inc_ref on an object that appears alive but becomes dead before the is_allocated check. The slot could be reused immediately by another allocation.

**Rustacean (Soundness 觀點):**
This is a clear memory safety violation. Calling dec_ref on a potentially reused slot violates Rust's safety guarantees. The fix is straightforward - don't call dec_ref when the slot may be invalid.

**Geohot (Exploit 觀點):**
An attacker could potentially trigger this race repeatedly to cause memory corruption and potentially achieve arbitrary code execution if they can control the timing and what gets allocated in the reused slot.