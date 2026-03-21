# [Bug]: IncrementalMarkState unsafe impl Sync enables UB when parallel marking is used

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Parallel marking must be explicitly enabled; not default behavior |
| **Severity (嚴重程度)** | `Critical` | Undefined behavior - memory safety violation possible |
| **Reproducibility (復現難度)** | `Low` | Requires enabling parallel marking and specific concurrent workload |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `IncrementalMarkState` (gc/incremental.rs)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

The `IncrementalMarkState` struct contains an `UnsafeCell<SegQueue>` field for the worklist, but implements `unsafe impl Sync` with justification that assumes single-threaded access. This is a "timebomb" bug: if parallel marking is enabled without adding proper synchronization to the worklist field, undefined behavior results.

### 預期行為 (Expected Behavior)

`IncrementalMarkState` should only be accessed from a single thread, OR proper synchronization should protect the `worklist` field when accessed from multiple threads.

### 實際行為 (Actual Behavior)

The `unsafe impl Sync` declaration at line 220 in `gc/incremental.rs` claims:

```rust
/// SAFETY: `IncrementalMarkState` is accessed as a process-level singleton via `global()`.
///
/// The `UnsafeCell<SegQueue>` in the `worklist` field is accessed single-threaded from the
/// GC thread during mark slices via `push_work()` and `pop_work()`. All other fields are
/// either atomic or protected by Mutex.
///
/// The blanket `unsafe impl Sync` is justified because:
/// 1. All access to `worklist` occurs from the GC thread during synchronized mark slices
/// 2. No concurrent access from mutator threads
/// 3. Atomic fields use proper ordering (`SeqCst` for writes, default for reads)
///
/// When parallel marking is implemented:
/// 1. The `worklist` field MUST be protected with proper synchronization
/// 2. Concurrent access without synchronization is undefined behavior
unsafe impl Sync for IncrementalMarkState {}
```

The comments explicitly state "When parallel marking is implemented: ... Concurrent access without synchronization is undefined behavior" but the `Sync` impl is already declared. This creates a soundness hole if parallel marking is enabled.

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. `IncrementalMarkState::worklist` is declared as `UnsafeCell<SegQueue<*const GcBox<()>>>` (line 153)
2. `UnsafeCell` Opts out of `Send` and `Sync` by default
3. The `unsafe impl Sync` declaration overrides this, claiming safety based on single-threaded access
4. `push_work()` (line 367) and `pop_work()` (line 372) access `worklist` through shared references (`&self`)
5. If multiple GC threads call these concurrently (parallel marking), multiple threads access the same `UnsafeCell` through shared references without synchronization
6. This is undefined behavior per Rust's memory model

The internal `SegQueue` from `crossbeam` is thread-safe, but accessing it through `UnsafeCell` via shared references from multiple threads is UB because `UnsafeCell` gives exclusive mutable access through its getter.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable parallel marking feature
2. Spawn multiple GC worker threads
3. Have them call `mark_slice()` concurrently which calls `pop_work()` on the same `IncrementalMarkState::global()`
4. Observe UB or crashes due to data race on `UnsafeCell`

```rust
// This would be UB if parallel marking is enabled and workers
// call mark_slice concurrently:
let state = IncrementalMarkState::global();
loop {
    if let Some(ptr) = state.pop_work() {  // Multiple threads calling this
        // process ptr
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option 1 (Recommended):** Remove `unsafe impl Sync` and add proper synchronization:
```rust
// Add Mutex or RwLock around worklist
worklist: Mutex<SegQueue<*const GcBox<()>>>,
```

**Option 2:** If single-threaded access is guaranteed by the architecture, add a compile-time assertion:
```rust
// Assert that we are not in multi-threaded context
const _: () = assert!(!cfg!(feature = "parallel_marking"), 
    "IncrementalMarkState requires synchronization when parallel marking is enabled");
```

**Option 3:** Use `Arc<SegQueue>` instead of `UnsafeCell<SegQueue>` and remove the `unsafe impl Sync`.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The worklist is critical for incremental marking coordination. The current design uses a concurrent queue (SegQueue from crossbeam) but wraps it in UnsafeCell, creating a soundness issue if parallel marking is enabled. The comment explicitly warns about this, suggesting the issue is known but deferred. In a proper GC design, all shared state between threads must be protected by synchronization primitives visible in the type system.

**Rustacean (Soundness 觀點):**
This is a textbook unsafe code soundness issue. The `unsafe impl Sync` claims safety based on an invariant ("single-threaded access") that is not enforced by the type system and could be violated by future code changes. The comment "When parallel marking is implemented, proper synchronization must be added" explicitly acknowledges the unsoundness. This is exactly the kind of latent UB that `unsafe` block audits should catch.

**Geohot (Exploit 觀點):**
If an attacker can enable parallel marking and cause concurrent access to the worklist, they could trigger the undefined behavior. The UnsafeCell accessed through shared references from multiple threads creates potential for memory corruption. Even if the current code only calls these methods from single-threaded contexts, a future refactor or bug in the parallel marking code could enable the exploit path.

---

## Resolution (2026-03-21)

**Outcome:** Invalid — the concurrent access scenario described does not exist.

Investigation of `marker.rs` confirms that `ParallelMarkCoordinator` (the parallel marking system) does not use `IncrementalMarkState` at all; it has its own separate work queues (`PerThreadMarkQueue`). The `IncrementalMarkState.worklist` is accessed exclusively from the GC thread during incremental mark slices — never from parallel worker threads. The `unsafe impl Sync` invariant (single-threaded worklist access) is upheld by the current architecture. No code change needed.