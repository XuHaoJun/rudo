# [Bug]: GcBoxWeakRef::upgrade 缺少驗證檢查 - 與 clone 行為不一致

**Status:** Open
**Tags:** Verified

---

## 📊 Threat Model Assessment

| Aspect | Assessment |
|--------|------------|
| Likelihood | Medium |
| Severity | High |
| Reproducibility | Medium |

---

## 🧩 Affected Component & Environment

- **Component:** `GcBoxWeakRef::upgrade()` (ptr.rs:506-558)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 Description

### Expected Behavior

`GcBoxWeakRef::upgrade()` should perform the same validation checks as `GcBoxWeakRef::clone()` before dereferencing the pointer. This includes alignment check, MIN_VALID_HEAP_ADDRESS check, is_gc_box_pointer_valid check, and is_allocated check.

### Actual Behavior

`GcBoxWeakRef::upgrade()` (ptr.rs:506-558) directly dereferences the loaded pointer at line 510 WITHOUT any validation:

```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire).as_option()?;  // line 507

    unsafe {
        let gc_box = &*ptr.as_ptr();  // line 510 - NO VALIDATION!
        // ... rest of function
    }
}
```

In contrast, `GcBoxWeakRef::clone()` (lines 562-616) performs comprehensive validation:

```rust
// 1. Alignment check (line 571)
if ptr_addr % alignment != 0 || ptr_addr < MIN_VALID_HEAP_ADDRESS {
    return Self { ptr: AtomicNullable::null() };
}

// 2. is_gc_box_pointer_valid check (line 577)
if !is_gc_box_pointer_valid(ptr_addr) {
    return Self { ptr: AtomicNullable::null() };
}

// 3. has_dead_flag check (line 589)
if gc_box.has_dead_flag() { ... }

// 4. dropping_state check (line 595)
if gc_box.dropping_state() != 0 { ... }

// 5. is_allocated check AFTER inc_weak (lines 603-611)
if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    if !(*header).is_allocated(idx) {
        (*ptr.as_ptr()).dec_weak();
        return Self { ptr: AtomicNullable::null() };
    }
}
```

---

## 🔬 Root Cause Analysis

When a Weak pointer is stored in data that may outlive the GC object, and lazy sweep runs concurrently:

1. Object A in slot is lazy swept (freed)
2. Object B is allocated in the same slot
3. Mutator calls `GcBoxWeakRef::upgrade()` on Object B's GcBoxWeakRef
4. The old pointer (now pointing to Object B's slot) passes all flag checks
5. Dereferences the slot - but it's Object B's data now!
6. Returns a Gc pointing to the wrong object OR reads invalid memory

---

## 💣 Steps to Reproduce / PoC

```rust
// Requires concurrent test environment:
// 1. Store GcBoxWeakRef in a data structure
// 2. Trigger lazy sweep to reclaim original object
// 3. Allocate new object in same slot
// 4. Call GcBoxWeakRef::upgrade()
// 5. Observe incorrect behavior (wrong object or invalid memory access)
```

---

## 🛠️ Suggested Fix / Remediation

Add validation checks to `GcBoxWeakRef::upgrade()`, matching `GcBoxWeakRef::clone()`:

```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire).as_option()?;
    
    // ADD: Validate pointer before dereferencing
    let ptr_addr = ptr.as_ptr() as usize;
    let alignment = std::mem::align_of::<GcBox<T>>();
    if ptr_addr % alignment != 0 || ptr_addr < MIN_VALID_HEAP_ADDRESS {
        return None;
    }
    
    if !is_gc_box_pointer_valid(ptr_addr) {
        return None;
    }

    unsafe {
        let gc_box = &*ptr.as_ptr();
        
        // Existing checks...
        if gc_box.is_under_construction() {
            return None;
        }
        if gc_box.has_dead_flag() {
            return None;
        }
        if gc_box.dropping_state() != 0 {
            return None;
        }
        
        // ... rest of function
        
        // ADD: is_allocated check after successful upgrade
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                GcBox::dec_ref(ptr.as_ptr());
                return None;
            }
        }
        
        Some(Gc { ... })
    }
}
```

---

## 🗣️ Internal Discussion Record

### R. Kent Dybvig
This is a consistency issue between two methods that should have the same validation behavior. The clone() method was updated with proper checks, but upgrade() was overlooked.

### Rustacean
The missing validation could lead to dereferencing invalid memory. While the subsequent flag checks (has_dead_flag, dropping_state) provide some protection, they don't catch all cases of slot reuse by lazy sweep.

### Geohot
An attacker controlling GC timing could trigger precise slot reuse to cause the upgrade() to return a Gc to the wrong object, potentially enabling further exploitation.
