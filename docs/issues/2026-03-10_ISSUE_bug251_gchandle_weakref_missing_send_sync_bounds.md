# [Bug]: GcBoxWeakRef/GcHandle/WeakCrossThreadHandle implement Send+Sync without requiring T: Send

**Status:** Invalid
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會在不經意的情況下跨執行緒傳遞 non-Send 類型 |
| **Severity (嚴重程度)** | High | 繞過 Rust 的 Send/Sync 類型系統，可能導致未定義行為 |
| **Reproducibility (復現難度)** | Low | 編譯器不會阻止此行為，需依賴人工審查 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef`, `GcHandle`, `WeakCrossThreadHandle`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`GcBoxWeakRef<T>`, `GcHandle<T>`, `WeakCrossThreadHandle<T>` 應該 only implement `Send` and `Sync` when `T: Send` or `T: Sync` respectively.

### 實際行為
These types unconditionally implement `Send` and `Sync` for any `T: Trace + 'static`:

```rust
// ptr.rs:751-753
unsafe impl<T: Trace + 'static> Send for GcBoxWeakRef<T> {}
unsafe impl<T: Trace + 'static> Sync for GcBoxWeakRef<T> {}

// cross_thread.rs:79-81
unsafe impl<T: Trace + 'static> Send for GcHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for GcHandle<T> {}

// cross_thread.rs:513-515
unsafe impl<T: Trace + 'static> Send for WeakCrossThreadHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for WeakCrossThreadHandle<T> {}
```

This allows `GcHandle<NonSendType>` to be sent to another thread, violating Rust's type safety guarantees.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The implementations use `T: Trace + 'static` as the bound instead of `T: Trace + Send + 'static` or `T: Trace + Sync + 'static`.

While there are runtime checks (origin thread validation), this bypasses Rust's compile-time safety guarantees. The proper pattern should require the appropriate trait bounds.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::rc::Rc;
use std::cell::RefCell;
use std::thread;

#[derive(Trace)]
struct NonSend {
    data: Rc<RefCell<i32>>, // Rc is not Send
}

fn main() {
    let gc = Gc::new(NonSend { data: Rc::new(RefCell::new(42)) });
    let handle = gc.cross_thread_handle();
    
    // This compiles but should not - handle can be sent to another thread
    // even though the underlying type is not Send
    let handle2 = move || {
        // handle is moved into this closure
        // This should not compile if T: Send is required
    };
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change the Send/Sync implementations to require appropriate bounds:

```rust
// For GcBoxWeakRef - needs T: Send for Send impl, T: Sync for Sync impl
unsafe impl<T: Trace + Send + 'static> Send for GcBoxWeakRef<T> {}
unsafe impl<T: Trace + Sync + 'static> Sync for GcBoxWeakRef<T> {}

// For GcHandle - same
unsafe impl<T: Trace + Send + 'static> Send for GcHandle<T> {}
unsafe impl<T: Trace + Sync + 'static> Sync for GcHandle<T> {}

// For WeakCrossThreadHandle - same  
unsafe impl<T: Trace + Send + 'static> Send for WeakCrossThreadHandle<T> {}
unsafe impl<T: Trace + Sync + 'static> Sync for WeakCrossThreadHandle<T> {}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is primarily a Rust type system issue, not a GC correctness issue. The runtime checks in resolve() do protect against data races. However, this bypasses the Rust safety net and could lead to issues if the runtime checks are bypassed or become inconsistent.

**Rustacean (Soundness 觀點):**
This is a soundness issue. The unsafe impl of Send/Sync without proper bounds violates Rust's aliasing rules. Even though there's runtime protection, this could lead to undefined behavior if used incorrectly.

**Geohot (Exploit 觀點):**
An attacker who finds a way to bypass the runtime thread check could potentially cause data races. The current implementation relies on "defense in depth" but the first line of defense (the type system) is bypassed.

---

## Resolution (2026-03-14)

**Outcome:** Invalid — deliberate design choice, not a bug.

The `Send + Sync` implementations without `T: Send`/`T: Sync` bounds are **intentional** and documented in `crates/rudo-gc/src/handles/cross_thread.rs`:

- Module docs (lines 8–9): "Cross-thread handles are `Send + Sync` even when `T` is not, enabling frameworks to schedule UI updates from async threads without requiring signal types to implement thread-safe traits."
- Safety argument (lines 14–29): (1) No direct access to `T` from non-origin threads; (2) `resolve()` enforces origin-thread affinity at runtime (panic if wrong thread); (3) Root registration keeps the object alive.

The handle is an opaque token — it stores `NonNull<GcBox<T>>`, `Weak<ThreadControlBlock>`, `ThreadId`, and `HandleId`. The only way to obtain `Gc<T>` (and thus access `T`) is via `resolve()`, which asserts `std::thread::current().id() == self.origin_thread` before any access. Non-origin threads cannot access `T`; they can only hold the handle.

Adding `T: Send`/`T: Sync` bounds would be a **breaking API change** that would remove the documented use case (e.g. `GcHandle<NonSendType>` for UI scheduling). No source code changes applied per Invalid-issue policy.
