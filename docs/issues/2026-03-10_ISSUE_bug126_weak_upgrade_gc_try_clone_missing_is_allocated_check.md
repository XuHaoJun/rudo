# [Bug]: Weak::upgrade and Gc::try_clone missing is_allocated check after ref increment

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires lazy sweep concurrent with upgrade/try_clone |
| **Severity (嚴重程度)** | Critical | Use-after-free leading to memory corruption |
| **Reproducibility (復現難度)** | High | Needs concurrent lazy sweep + upgrade/try_clone |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::upgrade`, `Gc::try_clone`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`Weak::upgrade` and `Gc::try_clone` are missing the `is_allocated` check after incrementing the reference count. This can lead to use-after-free when lazy sweep runs concurrently.

### 預期行為 (Expected Behavior)
After incrementing the reference count, the code should verify the slot has not been swept and reused (same as `Gc::clone`).

### 實際行為 (Actual Behavior)
- `Weak::upgrade` (ptr.rs:1870-1930): Post-CAS check only verifies `dropping_state` and `has_dead_flag`, but NOT `is_allocated`
- `Gc::try_clone` (ptr.rs:1296-1326): Post-increment check only verifies flags, but NOT `is_allocated`

Compare with `Gc::clone` (ptr.rs:1668-1704) which correctly checks `is_allocated` at lines 1691-1697.

---

## 🔬 根本原因分析 (Root Cause Analysis)

```rust
// Gc::clone (CORRECT - has is_allocated check)
(*gc_box_ptr).inc_ref();
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {  // <-- THIS CHECK
        GcBox::dec_ref(gc_box_ptr);
        panic!("Gc::clone: object slot was swept after inc_ref");
    }
}

// Gc::try_clone (BUG - missing is_allocated check)
if !(*gc_box_ptr).try_inc_ref_if_nonzero() { ... }
// NO is_allocated check here!

// Weak::upgrade (BUG - missing is_allocated check)
if gc_box.ref_count.compare_exchange_weak(...).is_ok() {
    // Post-CAS safety check
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() { ... }
    // NO is_allocated check here!
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable lazy sweep
2. Create a `Gc<T>` and then `Weak`
3. Drop the `Gc<T>`
4. While lazy sweep is running, call `Weak::upgrade()` or `Gc::try_clone()`
5. Race condition: slot may be reused between CAS and return

---

## 🛠️ 建議修復方案 (Suggested Fix)

Add `is_allocated` check after successful ref count increment in both functions:

```rust
// In Gc::try_clone, after line 1320:
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(gc_box_ptr);
        return None;
    }
}

// In Weak::upgrade, after line 1921:
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep creates a race window where a slot can be reused between ref count increment and the return. The BiBOP allocator must check `is_allocated` to ensure the slot still contains the original object.

**Rustacean (Soundness 觀點):**
This is a memory safety violation. The returned `Gc` may point to a newly allocated object in the reused slot, causing type confusion and potential undefined behavior.

**Geohot (Exploit 觀點):**
An attacker could exploit this to achieve arbitrary read/write by:
1. Creating a GC object with controlled content
2. Letting it be collected and slot reused
3. Upgrading a stale weak reference to get a pointer to the new object
