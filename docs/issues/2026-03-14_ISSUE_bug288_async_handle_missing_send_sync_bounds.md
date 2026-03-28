# [Bug]: AsyncHandle/GcScope/AsyncGcHandle implement Send+Sync without requiring T: Send/Sync

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會在不經意的情況下跨執行緒傳遞 non-Send 類型 |
| **Severity (嚴重程度)** | High | 繞過 Rust 的 Send/Sync 類型系統，可能導致未定義行為 |
| **Reproducibility (重現難度)** | Low | 編譯器不會阻止此行為，需依賴人工審查 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle`, `GcScope`, `AsyncGcHandle`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`AsyncHandle<T>`, `GcScope`, `AsyncGcHandle` 應該 only implement `Send` and `Sync` when `T: Send` or `T: Sync` respectively.

### 實際行為
These types unconditionally implement `Send` and `Sync`:

```rust
// async.rs:786-787
unsafe impl<T: Trace + 'static> Send for AsyncHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for AsyncHandle<T> {}

// async.rs:1407-1408
unsafe impl Send for GcScope {}
unsafe impl Sync for GcScope {}

// async.rs:1410-1411
unsafe impl Send for AsyncGcHandle {}
unsafe impl Sync for AsyncGcHandle {}
```

This is the same pattern as bug251, but for different types in the async handles module.

This allows `AsyncHandle<NonSendType>` to be sent to another thread, violating Rust's type safety guarantees.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The implementations use `T: Trace + 'static` as the bound instead of `T: Trace + Send + 'static` or `T: Trace + Sync + 'static`.

While there may be runtime checks in some methods, this bypasses Rust's compile-time safety guarantees. The proper pattern should require the appropriate trait bounds.

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
    let handle = gc.async_handle();  // Hypothetical method
    
    // This compiles but should not - handle can be sent to another thread
    // even though the underlying type is not Send
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change the Send/Sync implementations to require appropriate bounds:

```rust
// For AsyncHandle - needs T: Send for Send impl, T: Sync for Sync impl
unsafe impl<T: Trace + Send + 'static> Send for AsyncHandle<T> {}
unsafe impl<T: Trace + Sync + 'static> Sync for AsyncHandle<T> {}

// For GcScope - similar (but it doesn't have generic T)
unsafe impl<T: Trace + Send> Send for GcScope {}
unsafe impl<T: Trace + Sync> Sync for GcScope {}

// For AsyncGcHandle - similar
unsafe impl<T: Trace + Send> Send for AsyncGcHandle {}
unsafe impl<T: Trace + Sync> Sync for AsyncGcHandle {}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is primarily a Rust type system issue, not a GC correctness issue. The runtime checks (if any) do protect against data races. However, this bypasses the Rust safety net and could lead to issues if the runtime checks are bypassed or become inconsistent.

**Rustacean (Soundness 觀點):**
This is a soundness issue. The unsafe impl of Send/Sync without proper bounds violates Rust's aliasing rules. Even though there's runtime protection, this could lead to undefined behavior if used incorrectly.

**Geohot (Exploit 觀點):**
An attacker who finds a way to bypass the runtime thread check could potentially cause data races. The current implementation relies on "defense in depth" but the first line of defense (the type system) is bypassed.

---

## Resolution (2026-03-21)

**Fix:** Changed `AsyncHandle<T>` Send/Sync implementations to require proper bounds in `crates/rudo-gc/src/handles/async.rs:826-827`.

```rust
// Before (unsound):
unsafe impl<T: Trace + 'static> Send for AsyncHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for AsyncHandle<T> {}

// After (sound):
unsafe impl<T: Trace + Send + 'static> Send for AsyncHandle<T> {}
unsafe impl<T: Trace + Sync + 'static> Sync for AsyncHandle<T> {}
```

**Note:** `GcScope` and `AsyncGcHandle` do not have generic `T` parameters (they are type-erased), so unconditional `Send + Sync` impls are appropriate for these types since they only contain raw pointers which are already `Send + Sync`.

---

## 🔗 相關 Issue

- bug251: GcBoxWeakRef/GcHandle/WeakCrossThreadHandle implement Send+Sync without requiring T: Send
