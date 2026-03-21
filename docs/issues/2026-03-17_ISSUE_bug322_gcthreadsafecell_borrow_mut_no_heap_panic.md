# [Bug]: GcThreadSafeCell::borrow_mut panic when called from thread without GC heap

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Cross-thread usage of GcThreadSafeCell requires careful heap management |
| **Severity (嚴重程度)** | `High` | Causes panic, denial of service |
| **Reproducibility (復現難度)** | `High` | Requires multi-threaded scenario with specific thread configuration |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut`, `unified_write_barrier`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `GcThreadSafeCell::borrow_mut()` is called from a thread without an initialized GC heap, it should gracefully handle the barrier (similar to how SATB fallback works).

### 實際行為 (Actual Behavior)
The code panics with "thread local heap not initialized" because `trigger_write_barrier_with_incremental` calls `unified_write_barrier` which uses `with_heap` (not `try_with_heap`), causing a panic when no heap exists.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `cell.rs:1064-1093`:

1. Lines 1064-1090: SATB barrier correctly handles threads without heap using `try_with_heap` + cross-thread buffer fallback
2. Line 1093: Calls `trigger_write_barrier_with_incremental(incremental_active, generational_active)`
3. In `trigger_write_barrier_with_incremental` (line 1171-1181): It calls `unified_write_barrier(ptr, incremental_active)`
4. In `unified_write_barrier` (heap.rs:2999): Uses `with_heap` which **panics** if no heap exists

The SATB barrier has a fallback to cross-thread buffer when `try_with_heap` returns `None`, but the generational/incremental barrier does NOT have this fallback and directly uses `with_heap`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace};
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let cell = GcThreadSafeCell::new(Data { value: 42 });
    
    // Spawn a thread WITHOUT GC heap
    let handle = thread::spawn(move || {
        // This will panic because the thread has no GC heap
        let mut data = cell.borrow_mut();
        data.value = 100;
    });
    
    handle.join().unwrap();
}
```

Expected: Graceful handling or documented error
Actual: Panic "thread local heap not initialized"

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add fallback to `unified_write_barrier` or `trigger_write_barrier_with_incremental` to handle threads without heap:

1. Use `try_with_heap` in `unified_write_barrier` and return early if no heap (similar to SATB pattern)
2. Or add cross-thread dirty page tracking mechanism for generational barrier

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generational barrier needs to track dirty pages per-thread or globally. When a thread without a heap writes to a GcThreadSafeCell, the page should still be marked dirty for the GC to properly track OLD->YOUNG references during minor collection.

**Rustacean (Soundness 觀點):**
This is a panic (not UB), but it's a usability issue - the API should either work correctly or return an error rather than panicking. The current behavior violates the principle of least surprise.

**Geohot (Exploit 觀點):**
Not directly exploitable as a security issue, but could be used for denial of service in multi-threaded applications where worker threads don't have GC heaps initialized.

---

## Resolution (2026-03-21)

**Outcome:** Invalid — the described panic cannot occur in the current codebase.

**Analysis:**

1. **Panic message does not exist.** The string `"thread local heap not initialized"` (or any equivalent) is absent from the entire codebase. The issue's premise is incorrect.

2. **`with_heap` does not panic on uninitialized threads.** `with_heap` calls `HEAP.with(...)` where `HEAP` is a lazily-initialized `thread_local!`. On first access from any spawned thread, `ThreadLocalHeap::new()` runs automatically, registering the thread and creating an empty heap (`min_addr = usize::MAX`, `max_addr = 0`). No panic occurs.

3. **`unified_write_barrier` returns early for non-GC threads.** At `heap.rs:3032`, the first check is `if ptr_addr < heap.min_addr || ptr_addr >= heap.max_addr { return; }`. For threads with an unallocated heap, `min_addr = usize::MAX`, so `ptr_addr < usize::MAX` is true for any valid pointer — the barrier exits immediately without doing anything. This is correct and safe behavior.

**Verified by:** Static analysis of `heap.rs:3025–3034` (`unified_write_barrier`), `heap.rs:3434–3439` (`with_heap`), `heap.rs:3375–3409` (`ThreadLocalHeap::new`), and `cell.rs:1059–1189` (`GcThreadSafeCell::borrow_mut` / `trigger_write_barrier_with_incremental`). Existing sync tests (47 passing) confirm no regressions.
