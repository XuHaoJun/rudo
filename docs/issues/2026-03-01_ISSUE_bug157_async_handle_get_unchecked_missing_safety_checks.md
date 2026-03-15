# [Bug]: AsyncHandle::get_unchecked() missing safety checks - potential UAF and null dereference

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | * Caller 可能會在 slot 為空或物件已被回收後調用 |
| **Severity (嚴重程度)** | `Critical` | * 可能導致 UAF 或 null dereference，屬於 soundness bug |
| **Reproducibility (復現難度)** | `Medium` | * 需要精確控制 GC 時機和 scope 生命周期 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get_unchecked()` in `handles/async.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

The `get_unchecked()` method is an `unsafe` function that dereferences pointers **without any validity checks**, while the safe version `get()` has proper checks.

### 預期行為 (Expected Behavior)
`get_unchecked()` should perform minimal safety checks (like null check) while trusting the caller for scope validity. However, it currently performs NO checks at all.

### 實際行為 (Actual Behavior)
The method directly dereferences:
```rust
pub unsafe fn get_unchecked(&self) -> &T {
    let slot = unsafe { &*self.slot };
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    unsafe { &*gc_box_ptr }.value()
}
```

Without checking:
1. If `slot.as_ptr()` is null
2. If the GcBox has been deallocated/swept
3. If `has_dead_flag()` is set
4. If `dropping_state()` is non-zero
5. If `is_under_construction()` is true

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `get()` method at lines 582-593 has proper assertions:
```rust
let gc_box = &*gc_box_ptr;
assert!(
    !gc_box.has_dead_flag()
        && gc_box.dropping_state() == 0
        && !gc_box.is_under_construction(),
    "AsyncHandle::get: cannot access a dead, dropping, or under construction Gc"
);
```

However, `get_unchecked()` (lines 629-633) does not perform any of these checks. The documentation states the caller must ensure the scope is alive, but:
- Even if the scope is alive, the slot could have been set to null
- Even if the scope is alive, the GC could have collected the object and reused the memory

This is a **soundness bug** - callers cannot maintain the safety invariant even when following the documented requirements.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;

#[derive(Trace)]
struct Data { value: u64 }

fn main() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    // Force GC to collect the object
    rudo_gc::collect_full();
    
    // This should be UB - object was collected but we try to access it
    // Even though scope is still alive, the slot may be null or reused
    println!("{}", unsafe { handle.get_unchecked().value });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add at least a null check in `get_unchecked()`:

```rust
pub unsafe fn get_unchecked(&self) -> &T {
    let slot = unsafe { &*self.slot };
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    
    // At minimum, check for null
    assert!(
        !gc_box_ptr.is_null(),
        "AsyncHandle::get_unchecked: slot is null"
    );
    
    unsafe { &*gc_box_ptr }.value()
}
```

Consider adding more checks or documenting why they're not needed.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The GC may collect and reuse memory even while the scope is alive. The slot can be cleared by the GC during sweeping. Without null checks, dereferencing a null or reused pointer is undefined behavior.

**Rustacean (Soundness 觀點):**
This is a soundness bug. The safety invariant cannot be maintained - even if callers follow the documented requirements (scope alive), the slot can still be null or the memory reused. This violates Rust's memory safety guarantees.

**Geohot (Exploit 觀點):**
If an attacker can trigger GC at a precise moment between scope check and dereference, they could potentially:
- Cause null dereference (DoS)
- Cause UAF if memory is reused (potential code execution)

---

## Resolution (2026-03-02)

**Outcome:** Fixed.

Added safety checks to `AsyncHandle::get_unchecked()` in `handles/async.rs`:
1. Null check on `gc_box_ptr` before dereference
2. `has_dead_flag()` check
3. `dropping_state() == 0` check
4. `!is_under_construction()` check

Behavior now matches `get()` for GcBox state validation. Reproduction test added: `repro_bug157_async_handle_get_unchecked_valid_scope`.
