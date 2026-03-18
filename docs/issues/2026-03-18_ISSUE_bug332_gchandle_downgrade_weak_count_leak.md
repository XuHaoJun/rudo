# [Bug]: GcHandle::downgrade Weak Count Leak - dec_weak Not Called When Slot Swept

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Concurrent GC with lazy sweep can trigger this race window |
| **Severity (嚴重程度)** | Critical | Weak reference count leak leads to memory never being reclaimed |
| **Reproducibility (復現難度)** | Medium | Requires concurrent lazy sweep to trigger, but deterministic |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade`, `handles/cross_thread.rs`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `GcHandle::downgrade` detects the slot was swept (via `is_allocated` returning false) after `inc_weak` was called, it should call `dec_weak` to undo the `inc_weak` call, then return a null `WeakCrossThreadHandle`.

### 實際行為 (Actual Behavior)
When `is_allocated` returns false after `inc_weak` was called, the function returns a null weak handle WITHOUT calling `dec_weak`. This causes a weak reference count leak:
- Weak references never get cleaned up (memory leak)
- The object may never be reclaimed if only weak refs remain

This is the same bug pattern as bug331 but in a different location:
- Bug331: GcHandle::try_resolve_impl - ref_count leak
- This bug: GcHandle::downgrade - weak_count leak

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `GcHandle::downgrade` - TCB path (cross_thread.rs:366-379):

```rust
// Line 366: inc_weak is called
(*self.ptr.as_ptr()).inc_weak();

if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // BUG: Weak count leaked! dec_weak NOT called!
        drop(roots);
        return WeakCrossThreadHandle {
            weak: GcBoxWeakRef::null(),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        };
    }
}
```

Same bug in orphan path (cross_thread.rs:400-413).

The comment at line 372 claims "Don't call dec_weak - slot may be reused (bug133)" but this is a misunderstanding. The bug133 fix was about avoiding TOCTOU when checking `is_allocated` AFTER `inc_weak`, not about skipping `dec_weak`. This creates a new bug.

The race condition:
1. Thread calls `downgrade()` on a GcHandle
2. Object passes initial liveness checks
3. `inc_weak()` is called (line 366) - increments weak_count
4. **Between step 3 and the is_allocated check**, lazy sweep runs and reclaims the slot
5. `is_allocated` returns false (line 371)
6. Function returns null weak handle WITHOUT calling `dec_weak` - BUG!

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This requires concurrent lazy sweep:
1. Create GcHandle with object
2. Start background thread doing continuous lazy sweep
3. Main thread calls `downgrade()` in tight loop
4. Observe weak_count increasing without bound (memory leak)

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `dec_weak` before returning null when slot is not allocated:

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    (*self.ptr.as_ptr()).dec_weak();  // Undo the inc_weak
    drop(roots);
    return WeakCrossThreadHandle {
        weak: GcBoxWeakRef::null(),
        origin_tcb: Weak::clone(&self.origin_tcb),
        origin_thread: self.origin_thread,
    };
}
```

Apply to both TCB path (line 371) and orphan path (line 405).

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic reference count leak bug. The GC must ensure that every `inc_weak` has a corresponding `dec_weak`. The comment referencing "bug133" is a misapplication - that fix was about TOCTOU ordering, not about skipping reference count operations. This leak will cause objects with only weak references to never be collected, leading to memory growth over time.

**Rustacean (Soundness 觀點):**
This is a memory leak bug, not a safety violation (no UB). The weak reference count corruption scenario (slot reused by new object) could lead to incorrect weak reference behavior. The code incorrectly reasons that skipping `dec_weak` protects against slot reuse, but this reasoning is backwards - the proper fix is to always balance inc_weak/dec_weak.

**Geohot (Exploit 觀點):**
The exploitation path would require:
1. Controlling the timing of lazy sweep (difficult but possible with GcScheduler knobs)
2. Allocating a new object in the swept slot
3. The leaked weak_count would keep the old object alive even after all weak refs are dropped

This is the same root cause as bug331 but in a different code path.

---

## ✅ 修復記錄 (Fix Record)

- **Date:** 
- **Fix:**

---

## 🔍 驗證記錄 (Verification)

Confirmed bug exists in two locations:
1. `cross_thread.rs:371` - GcHandle::downgrade (TCB path)
2. `cross_thread.rs:405` - GcHandle::downgrade (orphan path)

Both locations call `inc_weak()` but don't call `dec_weak()` when `is_allocated` returns false.
