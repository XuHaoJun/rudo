# [Bug]: GcBoxWeakRef and WeakCrossThreadHandle Send/Sync bounds are unsound

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要 `T: Trace + 'static` 但 `T: !Send + !Sync` |
| **Severity (嚴重程度)** | `Critical` | 違反 Rust 記憶體安全保證，導致未定義行為 |
| **Reproducibility (復現難度)** | `Medium` | 可通过单元测试验证 Send/Sync bounds |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef` (ptr.rs:1035,1038), `WeakCrossThreadHandle` (cross_thread.rs:926,928)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

`GcBoxWeakRef<T>` 和 `WeakCrossThreadHandle<T>` 的 `Send + Sync` 实现声称当 `T: Trace + 'static` 时这些类型是 `Send + Sync`。这是**不正确的**，因为 `Trace` trait 与 `Send/Sync` 没有关系。

### 預期行為 (Expected Behavior)

`unsafe impl<T: Trace + Send + Sync + 'static> Send for GcBoxWeakRef<T> {}`
`unsafe impl<T: Trace + Send + Sync + 'static> Sync for GcBoxWeakRef<T> {}`

### 實際行為 (Actual Behavior)

```rust
// ptr.rs:1030-1038
// SAFETY: GcBoxWeakRef<T> is Send + Sync because:
// - ptr: AtomicNullable<GcBox<T>> is Send + Sync (atomic pointer)
// - generation: u32 is Send + Sync (plain u32)
// - T: Trace + 'static bound ensures no non-Send/Sync types in the generic  <-- WRONG!
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + 'static> Send for GcBoxWeakRef<T> {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + 'static> Sync for GcBoxWeakRef<T> {}
```

同样的问题出现在 `WeakCrossThreadHandle`:
```rust
// cross_thread.rs:926-928
unsafe impl<T: Trace + 'static> Send for WeakCrossThreadHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for WeakCrossThreadHandle<T> {}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`Trace` trait 只与垃圾回收相关，不提供任何 `Send/Sync` 保证。例如：

```rust
use std::cell::Cell;
use rudo_gc::{Gc, Trace};

// Cell<i32> implements Trace (trace.rs:411)
// Cell<i32> is NOT Send or Sync
let gc = Gc::new(Cell::new(42));
let weak = gc.weak_cross_thread_handle(); // Returns WeakCrossThreadHandle<Cell<i32>>

// weak is Send because of the incorrect bound!
// This allows sending Cell<i32> across threads, violating Cell's invariants
```

`Gc::new` 只要求 `T: Trace`（ptr.rs:1576 `impl<T: Trace> Gc<T>`），不要求 `T: Send + Sync`。
`weak_cross_thread_handle` 只要求 `T: Trace + 'static`（ptr.rs:2051）。

因此可以创建 `WeakCrossThreadHandle<Cell<i32>>`，且它会被错误地标记为 `Send`。

注意：虽然 `resolve()`、`try_resolve()`、`try_upgrade()` 都会检查 origin thread affinity（防止在错误线程调用），但 `unsafe impl` 的错误本身仍然违反了 Rust 的 soundness 要求。

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
use std::cell::Cell;
use rudo_gc::{Gc, Trace};

// This compiles because Cell<i32>: Trace
// But Cell<i32> is !Send + !Sync
let gc = Gc::new(Cell::new(42));
let weak = gc.weak_cross_thread_handle();

// This SHOULD fail to compile, but currently passes due to incorrect Send/Sync bounds
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
assert_send::<typeof(weak)>();  // Bug: compiles but shouldn't
assert_sync::<typeof(weak)>();  // Bug: compiles but shouldn't
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `GcBoxWeakRef` 的 Send/Sync bounds：
```rust
unsafe impl<T: Trace + Send + Sync + 'static> Send for GcBoxWeakRef<T> {}
unsafe impl<T: Trace + Send + Sync + 'static> Sync for GcBoxWeakRef<T> {}
```

修改 `WeakCrossThreadHandle` 的 Send/Sync bounds：
```rust
unsafe impl<T: Trace + Send + Sync + 'static> Send for WeakCrossThreadHandle<T> {}
unsafe impl<T: Trace + Send + Sync + 'static> Sync for WeakCrossThreadHandle<T> {}
```

同样检查其他可能有类似问题的类型。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GC weak reference 结构需要考虑跨线程安全性。虽然 `GcBoxWeakRef` 内部只存储指针和 generation，理论上可以安全地在线程间传递，但 Rust 的类型系统要求正确的 trait bounds。这个问题更多是 Rust 类型系统层面的问题，而非 GC 算法问题。

**Rustacean (Soundness 觀點):**
这是明确的 soundness bug。`unsafe impl` 的 SAFETY 注释声称 `T: Trace + 'static` 足以保证 Send/Sync，这是错误的。`Trace` trait 与 `Send/Sync` 没有语义关联。这是一个违反 Rust 核心安全保证的错误。

**Geohot (Exploit 觀點):**
虽然当前 API 通过 thread affinity checks 提供了运行时保护，但 `unsafe impl` 的错误仍然可能被恶意利用。如果有其他代码路径绕过了这些检查，可能导致 data race。Clippy 的 `non_send_fields_in_send_ty` 警告早就指出了这个问题。