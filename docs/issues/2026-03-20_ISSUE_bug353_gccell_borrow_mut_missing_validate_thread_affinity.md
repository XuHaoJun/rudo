# [Bug]: GcCell::borrow_mut() missing validate_thread_affinity causes cryptic panic

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Easy to accidentally call from wrong thread in async contexts |
| **Severity (嚴重程度)** | `Medium` | Panic with unhelpful error message instead of clear diagnostic |
| **Reproducibility (復現難度)** | `Low` | Trivially reproducible |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcCell::borrow_mut()` should check thread safety first using `validate_thread_affinity()`, providing a clear error message like other methods (`borrow()`, `borrow_mut_gen_only()`).

### 實際行為 (Actual Behavior)
`GcCell::borrow_mut()` directly calls `gc_cell_validate_and_barrier()` which uses `with_heap()`, causing a cryptic "thread local heap not initialized" panic when called from a thread without GC heap.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `cell.rs:155-213`, `borrow_mut()` does NOT call `validate_thread_affinity()` before doing barrier work:

```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    // BUG: No validate_thread_affinity() call here!

    let incremental_active = ...;
    let generational_active = ...;

    if incremental_active {
        // ...
        crate::heap::with_heap(|heap| {  // PANIC if no heap
            // ...
        });
    }

    if generational_active || incremental_active {
        crate::heap::gc_cell_validate_and_barrier(ptr, ...);  // Also uses with_heap
    }
    // ...
}
```

Compare to `borrow()` (line 134):
```rust
pub fn borrow(&self) -> Ref<'_, T> {
    self.validate_thread_affinity("borrow");  // CORRECT: validates first
    self.inner.borrow()
}
```

And `borrow_mut_gen_only()` (line 254):
```rust
pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T> {
    self.validate_thread_affinity("borrow_mut_gen_only");  // CORRECT: validates first
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, GcCell as GcCapture};
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let cell = GcCell::new(Data { value: 42 });
    
    // Spawn a thread WITHOUT GC heap
    let handle = thread::spawn(move || {
        // This should give clear error about thread safety violation
        // Actual: cryptic "thread local heap not initialized" panic
        *cell.borrow_mut() = Data { value: 100 };
    });
    
    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `self.validate_thread_affinity("borrow_mut");` at the beginning of `borrow_mut()`, before any barrier work.

```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    self.validate_thread_affinity("borrow_mut");  // Add this line
    
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    // ... rest of the method
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GcCell is designed to be thread-unsafe, requiring all access from the allocating thread. The validate_thread_affinity check is essential for catching misuse early with a clear error message.

**Rustacean (Soundness 觀點):**
This is a usability bug - the panic message doesn't help developers understand what went wrong. The fix is simple: add the missing validation call.

**Geohot (Exploit 觀點):**
Not a security issue, but the cryptic panic could mask other issues during debugging.
---

## Resolution (2026-03-21)

**Outcome:** Already fixed.

`GcCell::borrow_mut()` in `cell.rs:159` already calls `self.validate_thread_affinity("borrow_mut")` as the first statement, before any barrier work. The fix matches the suggested remediation exactly. Lib tests (`borrow_mut`) pass (3/3). No code changes needed.
