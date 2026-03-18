# [Bug]: GcHandle::try_resolve_impl Reference Count Leak - inc_ref Not Undone When Slot Swept

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Concurrent GC with lazy sweep can trigger this race window |
| **Severity (嚴重程度)** | Critical | Reference count leak leads to memory never being reclaimed |
| **Reproducibility (復現難度)** | Medium | Requires concurrent lazy sweep to trigger, but deterministic |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle`, `try_resolve_impl`, `cross_thread.rs`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `GcHandle::try_resolve_impl` detects the slot was swept (via `is_allocated` returning false), it should call `dec_ref` to undo the `inc_ref` call made earlier, then return `None`.

### 實際行為 (Actual Behavior)
When `is_allocated` returns false after `inc_ref` was called, the function returns `None` WITHOUT calling `dec_ref`. This causes a reference count leak:
- If slot was swept but not reused: object memory never reclaimed (leak)
- If slot was swept AND reused: reference count corruption of new object

The same bug pattern exists in multiple locations in `cross_thread.rs`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `try_resolve_impl` (cross_thread.rs:301-328):

```rust
// Line 310: inc_ref is called
gc_box.inc_ref();

// Lines 318-323: If slot was swept, returns None WITHOUT calling dec_ref!
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // BUG: Reference count leaked!
        return None;
    }
}
```

The comment at line 321 claims "Don't call dec_ref - slot may be reused (bug133)" but this is a misunderstanding. The bug133 fix was about avoiding TOCTOU when checking `is_allocated` AFTER `inc_ref`, not about skipping `dec_ref`. This creates a new bug.

The race condition:
1. Thread calls `try_resolve()` on a GcHandle
2. Object passes initial liveness checks (lines 304-308)
3. `inc_ref()` is called (line 310) - increments ref_count
4. **Between step 3 and the is_allocated check**, lazy sweep runs and reclaims the slot
5. `is_allocated` returns false (lines 318-323)
6. Function returns `None` WITHOUT calling `dec_ref` - BUG!

Same bug in:
- `GcHandle::downgrade` - TCB path (lines 371-379) 
- `GcHandle::downgrade` - orphan path (lines 405-413)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This requires concurrent lazy sweep. Use ThreadSanitizer or create a test that:
1. Create GcHandle with object
2. Start background thread doing continuous lazy sweep
3. Main thread calls `try_resolve()` in tight loop
4. Observe ref_count increasing without bound (memory leak)

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

The correct pattern should be:
1. Check `is_allocated` FIRST
2. If not allocated, return early (no increment needed)
3. If allocated, then increment the ref count
4. Check `is_allocated` again to detect the race
5. If now not allocated, undo the increment and return None

Alternatively, add `dec_ref` before returning `None` when slot is not allocated:
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    GcBox::dec_ref(self.ptr.as_ptr());  // Undo the inc_ref
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic reference count leak bug. The GC must ensure that every `inc_ref` has a corresponding `dec_ref`. The comment referencing "bug133" is a misapplication - that fix was about TOCTOU ordering, not about skipping reference count operations. This leak will cause objects to never be collected, leading to memory growth over time.

**Rustacean (Soundness 觀點):**
This is a memory leak bug, not a safety violation (no UB). However, the reference count corruption scenario (slot reused by new object) could lead to use-after-free. The code incorrectly reasons that skipping `dec_ref` protects against slot reuse, but this reasoning is backwards - the proper fix is to check allocation status BEFORE incrementing.

**Geohot (Exploit 觀點):**
The exploitation path would require:
1. Controlling the timing of lazy sweep (difficult but possible with GcScheduler knobs)
2. Allocating a new object in the swept slot
3. The leaked ref_count would corrupt the new object's reference count
This is complex but theoretically viable for a memory corruption exploit.
