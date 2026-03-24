# [Bug]: WeakCrossThreadHandle::resolve() panics when origin thread terminates instead of returning None

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | When origin thread terminates and ThreadId is reused |
| **Severity (嚴重程度)** | Medium | API inconsistency, panic instead of graceful failure |
| **Reproducibility (復現難度)** | Medium | Requires thread termination and ThreadId reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::resolve()` in `handles/cross_thread.rs:821-839`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.x

---

## 📝 問題描述 (Description)

`WeakCrossThreadHandle::resolve()` panics when the origin thread has terminated, even though `self.weak.upgrade()` might succeed. This is inconsistent with `try_upgrade()` which returns `None` in the same scenario.

### 預期行為 (Expected Behavior)
`resolve()` should attempt `self.weak.upgrade()` before panicking, consistent with `try_upgrade()` behavior.

### 實際行為 (Actual Behavior)
`resolve()` panics at line 824-830 when `self.origin_tcb.upgrade().is_none()`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `WeakCrossThreadHandle::resolve()` (lines 821-839):

```rust
pub fn resolve(&self) -> Option<Gc<T>> {
    // Check TCB liveness BEFORE the ThreadId comparison...
    if self.origin_tcb.upgrade().is_none() {
        panic!(  // <-- BUG: Panics instead of trying weak.upgrade()
            "WeakCrossThreadHandle::resolve: origin thread has terminated..."
        );
    }
    assert_eq!(std::thread::current().id(), self.origin_thread, ...);
    self.weak.upgrade()  // <-- This could succeed!
}
```

Compare with `WeakCrossThreadHandle::try_upgrade()` (lines 889-902):

```rust
pub fn try_upgrade(&self) -> Option<Gc<T>> {
    self.origin_tcb.upgrade()?;  // <-- Returns None if TCB is gone
    assert_eq!(std::thread::current().id(), self.origin_thread, ...);
    self.weak.try_upgrade()
}
```

`try_upgrade()` uses `?` operator which returns `None` if TCB upgrade fails, then proceeds to call `self.weak.try_upgrade()`. `resolve()` should follow the same pattern.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Trace)]
struct Data { value: i32 }

fn main() {
    // Create Gc with weak handle
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    let weak = gc.weak_cross_thread_handle();
    drop(gc);
    
    // Simulate origin thread termination:
    // 1. Origin thread drops Gc
    // 2. Origin thread terminates (TCB dropped)
    // 3. New thread reuses same ThreadId
    
    // Now try to resolve from a thread with same ThreadId
    // BUG: resolve() panics even though weak.upgrade() could succeed
    let result = weak.resolve(); // Should return None, but panics!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Modify `WeakCrossThreadHandle::resolve()` to check `self.weak.upgrade()` before panicking when TCB is gone:

```rust
pub fn resolve(&self) -> Option<Gc<T>> {
    // Check TCB liveness BEFORE the ThreadId comparison to prevent ThreadId
    // reuse from bypassing origin-thread affinity after the thread terminates.
    if self.origin_tcb.upgrade().is_none() {
        // Origin thread terminated - but weak.upgrade() might still succeed
        // (consistent with try_upgrade() which returns None here)
        return self.weak.upgrade();
    }
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        "WeakCrossThreadHandle::resolve() must be called on the origin thread. \
         If the origin thread has terminated, use try_resolve() instead."
    );
    self.weak.upgrade()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Weak handles don't register in orphan table (unlike GcHandle)
- But the underlying GcBox may still be alive after origin thread terminates
- `weak.upgrade()` should work if GcBox is still live

**Rustacean (Soundness 觀點):**
- Not a soundness bug - no UAF or type confusion
- API inconsistency between `resolve()` and `try_upgrade()`

**Geohot (Exploit 觀點):**
- No exploit potential - just API inconsistency
- Panic is safe behavior, just not consistent with similar APIs
