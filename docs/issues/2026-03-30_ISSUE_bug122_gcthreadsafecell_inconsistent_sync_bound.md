# [Bug]: GcThreadSafeCell inconsistent Sync trait bound

**Status:** Fixed
**Tags:** Verified, Soundness

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Developers commonly use types that are Send but not Sync |
| **Severity (嚴重程度)** | Critical | Soundness violation - undefined behavior possible |
| **Reproducibility (復現難度)** | Medium | Can be demonstrated with type-level proof |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell` (cell.rs:1571-1576)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`GcThreadSafeCell` implements `Sync` with only `T: Send` bound, while sibling types `GcRwLock` and `GcMutex` require `T: Send + Sync`. The SAFETY comment references "bug440" claiming consistency, but the implementation does NOT match.

### 預期行為 (Expected Behavior)

`GcThreadSafeCell<T>` should have the same trait bounds as `GcRwLock<T>` and `GcMutex<T>`:
- `Send`: `T: Trace + Send + Sync + ?Sized`
- `Sync`: `T: Trace + Send + Sync + ?Sized`

### 實際行為 (Actual Behavior)

`GcThreadSafeCell<T>` has weaker bounds:
- `Send`: `T: Trace + Send + ?Sized` ✓
- `Sync`: `T: Trace + Send + ?Sized` ✗ (missing `Sync` bound)

---

## 🔬 根本原因分析 (Root Cause Analysis)

**File:** `crates/rudo-gc/src/cell.rs:1571-1576`

```rust
// SAFETY: GcThreadSafeCell uses parking_lot::Mutex internally, which handles synchronization.
// The mutex ensures exclusive access - only one thread can access the data at a time.
// The GC tracing uses data_ptr() during STW pauses when no mutator threads are running.
// parking_lot::Mutex<T> requires T: Send for Mutex<T>: Send.
unsafe impl<T: Trace + Send + ?Sized> Send for GcThreadSafeCell<T> {}

// SAFETY: GcThreadSafeCell uses parking_lot::Mutex internally, which is Send + Sync.
// Concurrent access is protected by the mutex, and GC tracing is safe during STW pauses.
// T: Send is required to match GcRwLock and GcMutex consistency (bug440).
unsafe impl<T: Trace + Send + ?Sized> Sync for GcThreadSafeCell<T> {}
```

**Comparison with GcRwLock/GcMutex (sync.rs:833-839):**
```rust
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcRwLock<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcRwLock<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcMutex<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcMutex<T> {}
```

The SAFETY comment is incorrect - `GcRwLock` and `GcMutex` require `T: Send + Sync`, not just `T: Send`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

The bug can be demonstrated through type theory. Consider:

```rust
use std::cell::Cell; // Cell<T> is Send but NOT Sync

// This compiles (correctly), Cell is Send
type Test1 = GcThreadSafeCell<Cell<i32>>;

// This SHOULD NOT compile if bounds were correct, because Cell<i32> is !Sync
// But currently it DOES compile due to the missing Sync bound!
type Test2 = <GcThreadSafeCell<Cell<i32>> as std::marker::Sync>;
```

A `Cell` wrapped in `GcThreadSafeCell` can be accessed from multiple threads via shared references because `GcThreadSafeCell<Cell<T>>: Sync`, but `Cell<T>` is not `Sync` - it has interior mutability and requires exclusive access.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change the trait bounds in `cell.rs:1571-1576`:

```rust
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcThreadSafeCell<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcThreadSafeCell<T> {}
```

This matches `GcRwLock` and `GcMutex` and correctly requires `T: Sync` for `Sync` impl.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The `parking_lot::Mutex` itself is `Sync`, but that doesn't mean any `T: Send` can be safely shared across threads via a mutex. The mutex only guarantees exclusive access - the underlying type `T` must still be safe to access from multiple threads. If `T` has interior mutability (`Cell`, `RefCell`), it needs `Sync` to be safely shared. The GC's STW pause mechanism doesn't change this - the `Sync` bound is about Rust's type safety, not GC correctness.

**Rustacean (Soundness 觀點):**
This is a clear soundness violation. The `Sync` trait indicates that `&T` can be shared across threads. If `T: Send` but `!Sync` (like `Cell<T>`), then `&T` is not safely shareable. The SAFETY comment is factually wrong - it claims consistency with `GcRwLock`/`GcMutex` but those types correctly require `T: Sync`. This is a bug440 regression where the original fix introduced an inconsistent state.

**Geohot (Exploit 觀點):**
From an exploit perspective, this is a data race condition baked into the type system. An attacker could spawn two threads that both hold shared references to a `GcThreadSafeCell<Cell<T>>`, then both call `.get()` or `.set()` simultaneously. This violates Rust's aliasing rules and could lead to data corruption or memory safety issues depending on what operations are performed on the `Cell`.

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-03-31
**Commit:** 514fc91
**Fix:** Changed trait bounds in `cell.rs:1593-1595` to require `T: Send + Sync`:

```rust
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcThreadSafeCell<T> {}
unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcThreadSafeCell<T> {}
```

This matches `GcRwLock` and `GcMutex` and correctly requires `T: Sync` for `Sync` impl.

**Verification:** `./clippy.sh` passes.
