# [Bug]: Weak::clone has TOCTOU race causing potential use-after-free

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent Weak clone while object is being deallocated |
| **Severity (嚴重程度)** | High | Can cause use-after-free and undefined behavior |
| **Reproducibility (復現難度)** | High | Needs specific timing with concurrent drop |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>` clone implementation in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Weak::clone()` should safely increment the weak reference count without causing any memory safety issues, even when called concurrently with object destruction.

### 實際行為 (Actual Behavior)
`Weak::clone()` has a Time-Of-Check-Time-Of-Use (TOCTOU) race condition. It loads the pointer, performs alignment validation, then dereferences to increment weak count. Another thread can deallocate the object between the load and the dereference, causing a use-after-free.

### Code Location
`crates/rudo-gc/src/ptr.rs:1717-1739`

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `Weak::clone()` implementation:

```rust
impl<T: Trace> Clone for Weak<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Relaxed);  // Step 1: Load pointer
        if ptr.is_null() {
            return Self { ptr: AtomicNullable::null() };
        }
        let ptr_addr = ptr.as_ptr() as usize;
        let alignment = std::mem::align_of::<GcBox<T>>();
        if ptr_addr % alignment != 0 {  // Step 2: Validate alignment
            return Self { ptr: AtomicNullable::null() };
        }
        let gc_box_ptr = ptr.as_ptr();
        unsafe {
            (*gc_box_ptr).inc_weak();  // Step 3: Dereference - TOCTOU!
        }
        Self {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
        }
    }
}
```

**Race Condition Timeline:**
1. Thread A loads `ptr` from `self.ptr` (pointer to valid GcBox)
2. Thread B: The GcBox is deallocated (ref_count drops to 0, sweep reclaims memory)
3. Thread A: Dereferences `gc_box_ptr` to call `inc_weak()` - **USE-AFTER-FREE**

The `is_gc_box_pointer_valid()` check in `Weak::drop()` shows the correct pattern - validation should happen atomically with the dereference, not in separate steps.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::sync::Arc;
use std::thread;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct Data {
    value: AtomicUsize,
}

fn main() {
    let gc = Gc::new(Data { value: AtomicUsize::new(0) });
    let weak = Gc::downgrade(&gc);
    
    // Spawn threads that repeatedly clone the weak reference
    let handles: Vec<_> = (0..4).map(|_| {
        let weak_clone = weak.clone();
        thread::spawn(move || {
            for _ in 0..10000 {
                let _ = weak_clone.clone(); // Race here
            }
        })
    }).collect();
    
    // Concurrently drop the last strong reference
    drop(gc);
    collect_full();
    
    for h in handles {
        h.join().unwrap();
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

The fix should perform pointer validation and weak count increment atomically, similar to how `GcBoxWeakRef::clone()` handles this (though that also has the same issue).

Option 1: Use atomic compare-and-swap to atomically validate and increment:
```rust
fn clone(&self) -> Self {
    loop {
        let ptr = self.ptr.load(Ordering::Acquire);
        let Some(ptr) = ptr.as_option() else {
            return Self { ptr: AtomicNullable::null() };
        };
        
        // Validate before dereferencing
        let ptr_addr = ptr.as_ptr() as usize;
        let alignment = std::mem::align_of::<GcBox<T>>();
        if ptr_addr % alignment != 0 || ptr_addr < 4096 {
            return Self { ptr: AtomicNullable::null() };
        }
        
        // Use try_inc_weak or similar atomic operation
        // SAFETY: After validation, ptr is likely valid
        unsafe {
            (*ptr.as_ptr()).inc_weak();
        }
        
        // Re-check pointer hasn't changed (handle races)
        let current = self.ptr.load(Ordering::Acquire);
        if current.as_ptr() == ptr.as_ptr() {
            return Self { ptr: AtomicNullable::new(ptr) };
        }
        // Pointer changed, need to undo and retry
        unsafe {
            (*ptr.as_ptr()).dec_weak();
        }
    }
}
```

Option 2: Use the same pattern as `is_gc_box_pointer_valid()` before dereferencing.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This TOCTOU pattern is dangerous in a GC context. The weak count is critical for determining when an object can be collected. If the race causes incorrect weak count, it could lead to premature collection (if count is too low) or memory leaks (if count is too high). The current validation check (alignment) is insufficient because it doesn't verify the object is still alive.

**Rustacean (Soundness 觀點):**
This is a clear memory safety violation. The `unsafe` block dereferences a pointer that may no longer be valid. Under Stacked Borrows, this could cause UB. The validation should be integrated with the atomic operation, not done as separate steps. The pattern of "load-validate-dereference" is inherently racy.

**Geohot (Exploit 觀點):**
This is exploitable in theory. If an attacker can control the timing, they could:
1. Create a GC object with a Weak reference
2. Use-after-free to read/write the freed memory before it's reallocated
3. The alignment check (`ptr_addr % alignment != 0`) is weak - it only catches obviously invalid pointers, not freed-but-reallocated ones
