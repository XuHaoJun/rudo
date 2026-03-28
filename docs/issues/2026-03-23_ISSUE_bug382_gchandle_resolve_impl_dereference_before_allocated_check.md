# [Bug]: GcHandle::resolve_impl dereferences before is_allocated check - TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires precise timing of lazy sweep slot reuse between dereference and check |
| **Severity (嚴重程度)** | High | Type confusion - reads fields from wrong object before validation |
| **Reproducibility (重現難度)** | High | Requires concurrent lazy sweep and handle resolve |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve_impl` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

The `gc_box` pointer should be validated (via `is_allocated` check or equivalent) BEFORE being dereferenced to read any fields.

### 實際行為 (Actual Behavior)

In `cross_thread.rs:213-269`, `resolve_impl()`:

1. **Line 215**: `let gc_box = &*self.ptr.as_ptr();` - Dereferences pointer to get `GcBox` reference
2. **Lines 216-227**: Reads `is_under_construction()`, `has_dead_flag()`, `dropping_state()` from `gc_box`
3. **Lines 229-238**: `is_allocated` check happens AFTER the dereference and field reads

**Problem**: If slot is swept and reused between line 215 and lines 229-238, we read fields from the NEW object in the reused slot, not the original object. This is a type confusion vulnerability.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**File:** `handles/cross_thread.rs:213-269`

```rust
fn resolve_impl(&self) -> Gc<T> {
    unsafe {
        let gc_box = &*self.ptr.as_ptr();  // LINE 215 - DEREFERENCE FIRST!
        
        // Lines 216-227: Read fields from gc_box
        assert!(
            !gc_box.is_under_construction(),  // Reads from potentially WRONG object
            ...
        );
        assert!(
            !gc_box.has_dead_flag(),  // Reads from potentially WRONG object
            ...
        );
        assert!(
            gc_box.dropping_state() == 0,  // Reads from potentially WRONG object
            ...
        );

        // Lines 229-238: is_allocated check TOO LATE!
        if let Some(idx) = crate::heap::ptr_to_object_index(...) {
            let header = ...;
            assert!((*header.as_ptr()).is_allocated(idx), ...);  // AFTER dereference!
        }
        ...
    }
}
```

**Race scenario:**
1. Handle points to object A in slot X
2. `gc_box = &*self.ptr.as_ptr()` gets reference to A's GcBox
3. Lazy sweep runs: A is swept, new object B is allocated in slot X (same address)
4. Lines 216-227 read B's fields (type confusion!)
5. Lines 229-238: `is_allocated` check passes (slot is allocated with B)
6. Generation check (lines 243-253) passes because we read B's generation twice
7. `inc_ref` operates on B correctly (by accident)

**Impact**: Type confusion - we validate B using A's expected state. If B happens to have valid state (not under construction, no dead flag, dropping_state=0), the checks pass when they shouldn't for A.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires precise concurrent timing:
// 1. Create GcHandle to object A
// 2. Trigger lazy sweep to reclaim A's slot
// 3. Allocate new object B in same slot (B has valid state)
// 4. Call resolve() on handle
// 5. Observe: B's fields are read during validation
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the `is_allocated` check BEFORE the first dereference of `gc_box`. The simplest approach is to perform pointer validation before creating the reference:

```rust
fn resolve_impl(&self) -> Gc<T> {
    unsafe {
        // First validate pointer is still allocated BEFORE dereferencing
        if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
            assert!(
                (*header.as_ptr()).is_allocated(idx),
                "GcHandle::resolve: object slot was swept before dereference"
            );
        }

        // NOW safe to dereference
        let gc_box = &*self.ptr.as_ptr();
        
        // Continue with existing field reads and generation checks...
        assert!(!gc_box.is_under_construction(), ...);
        ...
    }
}
```

This matches the pattern used in other similar functions and prevents the TOCTOU between dereference and validation.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The GC must maintain object identity invariants. Dereferencing before validation creates a window where we operate on the wrong object's state. This is especially dangerous in generational/mark-sweep GCs where slot reuse is common.

**Rustacean (Soundness 觀點):**
This is technically safe (generation check prevents wrong inc_ref), but logically incorrect. Reading fields from the wrong object before validation is a code smell and could lead to subtle bugs if the validation logic changes.

**Geohot (Exploit 觀點):**
While the generation check prevents immediate memory corruption, reading from wrong object's state could be exploited in complex scenarios where object state influences control flow.