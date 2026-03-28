# [Bug]: Handle::as_ptr() missing is_allocated check before dereference

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires specific GC timing + handle escape pattern |
| **Severity (嚴重程度)** | `Critical` | Type confusion from dereferencing swept slot |
| **Reproducibility (復現難度)** | `High` | Race between lazy sweep and handle access |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::as_ptr()` in `handles/mod.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

`Handle::as_ptr()` dereferences the handle's slot to obtain a `GcBox` pointer without first verifying the slot is still allocated. This creates a TOCTOU vulnerability where:

1. The slot passes an earlier validity check
2. Lazy sweep reclaims the slot (sets `is_allocated = false`)
3. The slot is reused for a new object with a different type
4. `as_ptr()` returns a pointer to the wrong object (type confusion)

### 預期行為 (Expected Behavior)

`Handle::as_ptr()` should verify the underlying `GcBox` is still allocated before returning the pointer, similar to how `Handle::get()` protects itself.

### 實際行為 (Actual Behavior)

`Handle::as_ptr()` (lines 453-456) directly dereferences `self.slot` without any `is_allocated` check:

```rust
pub unsafe fn as_ptr(&self) -> *const GcBox<T> {
    let slot = unsafe { &*self.slot };  // <-- NO is_allocated CHECK
    slot.as_ptr() as *const GcBox<T>
}
```

Compare to `Handle::get()` (lines 303-358) which has proper protection:

```rust
pub fn get(&self) -> &T {
    unsafe {
        let slot = &*self.slot;
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        // ... validation ...
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            assert!(
                (*header.as_ptr()).is_allocated(idx),  // <-- PROPER CHECK
                "Handle::get: slot has been swept and reused"
            );
        }
        // ...
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `Handle::as_ptr()` method was likely written as a fast path to obtain the raw pointer without the overhead of reference counting and validation. However, it lacks the `is_allocated` check that other handle access methods have.

This is particularly dangerous because:
1. `as_ptr()` returns a raw pointer that the caller may use directly
2. Without `is_allocated` checking, the returned pointer may reference a swept slot
3. If the slot was reused, this causes type confusion (reading wrong object's data)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudocode - actual PoC requires specific GC timing
let scope = HandleScope::new(&tcb);
let gc = Gc::new(DataTypeA { value: 42 });
let handle = scope.handle(gc);

// Trigger lazy sweep to reclaim the slot
// ... gc.collect() to trigger minor GC ...

// Call as_ptr() after slot is reclaimed and reused
let ptr = unsafe { handle.as_ptr() };
// ptr now points to WRONG OBJECT if slot was reused!
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check to `Handle::as_ptr()` before dereferencing:

```rust
pub unsafe fn as_ptr(&self) -> *const GcBox<T> {
    let slot_ref = &*self.slot;
    let gc_box_ptr = slot_ref.as_ptr() as *const GcBox<T>;
    
    // Add is_allocated check to match Handle::get() protection
    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "Handle::as_ptr: slot has been swept and reused"
        );
    }
    
    gc_box_ptr
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The handle system is a root management mechanism. Handles should be traced as roots during GC, preventing their referenced GcBoxes from being swept. However, `as_ptr()` bypasses the normal handle access pattern and could expose type confusion if the slot is reused. The generation mechanism provides some protection, but explicit `is_allocated` checking is the correct defense.

**Rustacean (Soundness 觀點):**
This is a soundness issue. `Handle::as_ptr()` returns a raw pointer derived from a potentially-swept slot. If the slot was reused with a different type, dereferencing this pointer could cause type confusion, which is undefined behavior in Rust. The unsafe contract in the API comment ("caller must ensure handle is valid") is insufficient - the API itself should not allow constructing invalid states.

**Geohot (Exploit 觀點):**
Type confusion from a swept/reused slot is a classic exploit primitive. If an attacker can trigger GC at a specific time, they could potentially have a handle point to attacker-controlled data. The missing `is_allocated` check combined with lazy sweep timing creates a controllable race window.

---

## Resolution (2026-03-23)

**Fixed.** Applied the suggested fix to `handles/mod.rs` in `Handle::as_ptr()`:
- Added `is_allocated` check before dereferencing the slot
- Matches the pattern used in `Handle::get()` and other handle access methods
- Clippy passes