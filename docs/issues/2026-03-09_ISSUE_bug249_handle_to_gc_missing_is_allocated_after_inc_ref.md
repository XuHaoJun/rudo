# [Bug]: Handle::to_gc and AsyncHandle::to_gc missing is_allocated check after ref increment (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent GC sweep during handle-to-gc conversion |
| **Severity (嚴重程度)** | Critical | Use-after-free, returning Gc to wrong (reallocated) object |
| **Reproducibility (復現難度)** | High | Requires precise timing of concurrent GC sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::to_gc()`, `AsyncHandle::to_gc()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Handle::to_gc()` and `AsyncHandle::to_gc()` should verify the slot is still allocated AFTER incrementing the ref count but BEFORE returning the Gc. This pattern is already implemented in `GcHandle::resolve()` at `cross_thread.rs:208-216`.

### 實際行為 (Actual Behavior)
After `try_inc_ref_if_nonzero()` successfully increments the ref count, the object could be swept by GC and the slot reallocated to a new object. Without an `is_allocated` check, the code returns a `Gc` pointing to the newly allocated (wrong) object, causing:
1. Use-after-free on the old object
2. Gc pointing to the wrong object (memory corruption)

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `handles/mod.rs:357-367` (Handle::to_gc):
```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("Handle::to_gc: object is being dropped by another thread");
}
// MISSING: is_allocated check here!
Gc::from_raw(gc_box_ptr as *const u8)
```

In `handles/async.rs:719-729` (AsyncHandle::to_gc):
```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::to_gc: object is being dropped by another thread");
}
// MISSING: is_allocated check here!
Gc::from_raw(gc_box_ptr as *const u8)
```

Compare with correct pattern in `cross_thread.rs:208-216` (GcHandle::resolve):
```rust
gc_box.inc_ref();

if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
        panic!("GcHandle::resolve: object slot was swept after inc_ref");
    }
}

Gc::from_raw(self.ptr.as_ptr() as *const u8)
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Create a `HandleScope` with a `Gc<T>`
2. Get a `Handle` from the scope
3. Call `handle.to_gc()` - this increments ref count
4. Concurrent GC sweep runs and reclaims the object
5. GC allocates a new object in the same slot
6. `to_gc()` returns Gc pointing to new (wrong) object

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add is_allocated check after `try_inc_ref_if_nonzero()` in both `Handle::to_gc()` and `AsyncHandle::to_gc()`:

```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("Handle::to_gc: object is being dropped by another thread");
}

// Check if slot was swept after ref increment
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(gc_box_ptr.cast_mut());
        panic!("Handle::to_gc: object slot was swept after inc_ref");
    }
}

Gc::from_raw(gc_box_ptr as *const u8)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The is_allocated check after ref increment is critical for lazy sweep GCs. Between successful ref increment and Gc::from_raw(), the sweeper could reclaim the old object and reallocate the slot. The returned Gc would then point to the wrong object. This is the same pattern required in GcHandle::resolve().

**Rustacean (Soundness 觀點):**
Returning a Gc to a reallocated object is undefined behavior - it creates a Gc that appears valid but points to completely different data. This violates memory safety invariants.

**Geohot (Exploit 觀點):**
If an attacker can control the timing of allocation after sweep, they could potentially create a "fake" object at the reallocated slot with controlled content, then trigger the buggy code path to get a Gc to it.

---

## 🔗 相關 Issue

- bug195: Handle::get / Handle::to_gc missing is_allocated check (different - before dereferencing)
- bug196: AsyncHandle::get / to_gc missing is_allocated check (different - before dereferencing)
- bug210: Handle::to_gc missing post-increment safety check (different - about dead_flag/dropping_state)

---

## Resolution (2026-03-14)

**Outcome:** Already fixed.

The fix was applied in a prior commit. The current implementations in `handles/mod.rs` (lines 382–388) and `handles/async.rs` (lines 755–761) both include the `is_allocated` check after `try_inc_ref_if_nonzero()`:

```rust
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::to_gc: object slot was swept after inc_ref"
    );
}
```

Behavior now matches the pattern in `GcHandle::resolve()` as described in the issue.
