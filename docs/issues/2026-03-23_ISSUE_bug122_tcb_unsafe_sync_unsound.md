# [Bug]: ThreadControlBlock unsafe impl Sync may be unsound due to UnsafeCell<LocalHeap>

**Status:** Fixed
**Tags:** Verified, Soundness
**Fixed by:** `841bf73` — "fix(sync): add explicit unsafe impl Sync for LocalHeap and LocalHandles (bug122)"

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Low` | Requires specific concurrent access patterns during non-STW periods |
| **Severity (嚴重程度)** | `Critical` | Potential undefined behavior from unsound Sync impl |
| **Reproducibility (復現難度)** | `Very High` | Cannot reliably reproduce; relies on specific memory ordering |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ThreadControlBlock` in `heap.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `current`

---

## 📝 問題描述 (Description)

`ThreadControlBlock` contains `UnsafeCell<LocalHeap>` and is marked unconditionally as `Send + Sync` via:
```rust
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for ThreadControlBlock {}
unsafe impl Sync for ThreadControlBlock {}
```

The `LocalHeap` struct contains multiple fields with interior mutability:
- `pages: Vec<NonNull<PageHeader>>`
- `small_pages: HashSet<usize>`
- `large_object_map: HashMap<usize, (usize, usize, usize)>`
- Various other `Vec` fields

### 預期行為 (Expected Behavior)
`unsafe impl Sync for ThreadControlBlock` should only be sound if `LocalHeap` is actually `Sync`. The `Sync` marker promises that `&ThreadControlBlock` can be safely shared across threads.

### 實際行為 (Actual Behavior)
The `UnsafeCell<LocalHeap>` wrapped in `ThreadControlBlock` makes `Sync` impl potentially unsound if `LocalHeap` is not actually `Sync`. While `Vec<T>` and `HashMap<K,V>` are `Sync` when their contents are `Sync`, the presence of `#[allow(clippy::non_send_fields_in_send_ty)]` suggests awareness of potential issues.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `Sync` impl for `ThreadControlBlock` relies on `LocalHeap` being `Sync`. However:

1. `LocalHeap` contains `Vec` and `HashMap` which use `UnsafeCell` internally
2. While standard library `Vec<T>` is `Sync` when `T: Sync`, the clippy lint `non_send_fields_in_send_ty` suggests concern about `UnsafeCell` wrapped in another `UnsafeCell`
3. The safety invariant is "only accessed during STW pauses" but Rust's type system cannot enforce temporal invariants

The SAFETY comment at `IncrementalMarkState`'s `Sync` impl states:
> "IncrementalMarkState is accessed as a process-level singleton via global(). All access to worklist occurs from the GC thread during synchronized mark slices"

This same guarantee does NOT exist for `ThreadControlBlock::heap` field.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

PoC would require:
1. Creating a `&ThreadControlBlock` reference accessible from multiple threads
2. Accessing `heap` field through shared reference during non-STW period
3. Demonstrating data race or UB

This is difficult to reproduce reliably as it depends on specific GC state and memory ordering.

```rust
// Hypothetical - not a working PoC
fn exploit() {
    //假设我们可以从另一个线程访问ThreadControlBlock的堆
    let tcb_ref: &ThreadControlBlock = /* ... */;
    // 在非STW期间通过shared reference访问heap
    // 这可能导致data race on LocalHeap's internal Vec/HashMap
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **Verify `LocalHeap` is truly `Sync`** - Add explicit `unsafe impl Sync for LocalHeap {}` with clear SAFETY documentation, OR
2. **Remove `Sync` impl** - If `LocalHeap` is not `Sync`, remove `unsafe impl Sync for ThreadControlBlock` and add proper synchronization for any cross-thread access
3. **Use `Mutex<LocalHeap>`** - If shared access is needed, wrap `LocalHeap` in a `Mutex` or `RwLock`
4. **Add compile-time verification** - Consider using `static_assertions` crate to verify trait bounds at compile time

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
LocalHeap的GC设计假设每个线程的堆只能从该线程访问，除了在STW暂停期间由GC线程访问。这个协议在Rust的类型系统中无法强制执行。如果`LocalHeap`被标记为`Sync`，则表面上是安全的，但实际上依赖于调用者遵守"仅在STW期间共享访问"的不成文规定。

**Rustacean (Soundness 觀點):**
`unsafe impl Sync for ThreadControlBlock` 声称`&ThreadControlBlock`可以安全地在线程间共享。这个声明要求`LocalHeap`是`Sync`的。虽然`Vec<T>`和`HashMap<K,V>`当它们的元素是`Sync`时是`Sync`的，但`UnsafeCell`的组合（通过`Vec`内部和`ThreadControlBlock.heap`）创造了潜在的问题。`#[allow(clippy::non_send_fields_in_send_ty)]`表明存在已知的lint问题。

**Geohot (Exploit 觀點):**
如果`LocalHeap`不是真正的`Sync`，攻击者可能利用这个缺陷通过在GC期间在线程间共享`ThreadControlBlock`引用来触发未定义行为。这可能导致：1) 对`LocalHeap`内部Vec/HashMap的data race，2) 释放后使用/双重释放如果堆被错误地同步。
