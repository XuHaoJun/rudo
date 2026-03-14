# [Bug]: Gc::try_clone missing is_allocated check after ref increment (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent GC sweep during try_clone |
| **Severity (嚴重程度)** | Critical | Use-after-free, returning Gc to wrong (reallocated) object |
| **Reproducibility (復現難度)** | High | Requires precise timing of concurrent GC sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::try_clone()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Gc::try_clone()` should verify the slot is still allocated AFTER incrementing the ref count but BEFORE returning the Gc. This pattern is already implemented in `Gc::clone()` at `ptr.rs:1691-1697`.

### 實際行為 (Actual Behavior)
After `try_inc_ref_if_nonzero()` successfully increments the ref count, the object could be swept by GC and the slot reallocated to a new object. Without an `is_allocated` check, the code returns a `Gc` pointing to the newly allocated (wrong) object, causing:
1. Use-after-free on the old object
2. Gc pointing to the wrong object (memory corruption)

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `ptr.rs:1310-1326` (Gc::try_clone):
```rust
// Avoid TOCTOU with concurrent drop: atomically increment only when ref_count > 0.
if !(*gc_box_ptr).try_inc_ref_if_nonzero() {
    return None;
}
// Post-increment safety check: dropping/dead may flip between pre-check and ref bump.
if (*gc_box_ptr).has_dead_flag()
    || (*gc_box_ptr).dropping_state() != 0
    || (*gc_box_ptr).is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr);
    return None;
}
// MISSING: is_allocated check here!
Some(Self {
    ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    _marker: PhantomData,
})
```

Compare with correct pattern in `ptr.rs:1691-1697` (Gc::clone):
```rust
(*gc_box_ptr).inc_ref();

if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(gc_box_ptr);
        panic!("Gc::clone: object slot was swept after inc_ref");
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Create a `Gc<T>` object
2. Call `Gc::try_clone(&gc)` - this increments ref count
3. Concurrent GC sweep runs and reclaims the object
4. GC allocates a new object in the same slot
5. `try_clone()` returns Gc pointing to new (wrong) object

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add is_allocated check after `try_inc_ref_if_nonzero()` in `Gc::try_clone()`:

```rust
// Avoid TOCTOU with concurrent drop: atomically increment only when ref_count > 0.
if !(*gc_box_ptr).try_inc_ref_if_nonzero() {
    return None;
}
// Post-increment safety check: dropping/dead may flip between pre-check and ref bump.
if (*gc_box_ptr).has_dead_flag()
    || (*gc_box_ptr).dropping_state() != 0
    || (*gc_box_ptr).is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr);
    return None;
}
// Check if slot was swept after ref increment
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    letheap::ptr_to_page_header(gc header = crate::_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(gc_box_ptr);
        return None;
    }
}
Some(Self {
    ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    _marker: PhantomData,
})
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The is_allocated check after ref increment is critical for lazy sweep GCs. Between successful ref increment and returning the Gc, the sweeper could reclaim the old object and reallocate the slot. The returned Gc would then point to the wrong object. This is the same pattern required in Gc::clone().

**Rustacean (Soundness 觀點):**
Returning a Gc to a reallocated object is undefined behavior - it creates a Gc that appears valid but points to completely different data. This violates memory safety invariants. Unlike Gc::clone() which panics on this condition, try_clone should return None.

**Geohot (Exploit 觀點):**
If an attacker can control the timing of allocation after sweep, they could potentially create a "fake" object at the reallocated slot with controlled content, then trigger the buggy code path to get a Gc to it.

---

## 🔗 相關 Issue

- bug140: Gc::try_clone TOCTOU panic (different - about panic vs return None)
- bug249: Handle::to_gc and AsyncHandle::to_gc missing is_allocated check (similar pattern but different functions)

---

## Resolution (2026-03-14)

**Verified fixed.** `ptr.rs` lines 1408–1416 now include the `is_allocated` check after successful `try_inc_ref_if_nonzero()`, matching the pattern in `Gc::clone()`. When the slot was swept, the code returns `None` without calling `dec_ref` (per bug133 — slot may be reused). `test_try_clone` passes.
