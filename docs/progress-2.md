# rudo-gc Implementation Progress Analysis

This document compares the original design document (2026-01-01 Gemini conversation) against the current `rudo-gc` implementation to identify what has been completed and what remains to be done.

## Summary

The core GC functionality is **substantially complete**. The BiBOP allocator, conservative stack scanning, mark-sweep collection, multi-threaded coordination, and derive macros are all implemented. The main gaps are in advanced features like incremental/concurrent marking and full cross-platform support.

---

## ✅ Completed Features

### 1. BiBOP Allocator with TLABs (Design Level 3)

**Status: Complete**

- Size classes: 16, 32, 64, 128, 256, 512, 1024, 2048 bytes
- Thread-Local Allocation Buffers (`Tlab` struct) with bump-pointer allocation
- `PageHeader` with mark bitmap, dirty bitmap, and allocated bitmap
- Compile-time size class routing via `const fn compute_size_class()`
- Large Object Space (LOS) for objects > 2KB with multi-page support

```rust
// From heap.rs - Size classes implemented
pub const SIZE_CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
```

### 2. Conservative Stack Scanning (Design Level 1)

**Status: Complete (Linux)**

- Register spilling via inline assembly (`spill_registers_and_scan`)
- Stack bounds detection via `pthread_getattr_np` on Linux
- Interior pointer support for large objects
- Address Space Coloring with `HEAP_HINT_ADDRESS` at `0x6000_0000_0000`
- Page quarantining ("blacklisting") when stack conflicts detected
- BiBOP-based O(1) object lookup from interior pointer

```rust
// From heap.rs - Address Space Coloring
pub const HEAP_HINT_ADDRESS: usize = 0x6000_0000_0000;
```

### 3. Trace Trait & Derive Macro (Design Level 4)

**Status: Complete**

- `#[derive(Trace)]` procedural macro for structs and enums
- Implementations for all std primitive types
- Implementations for std collections (`Vec`, `HashMap`, `Option`, etc.)
- Implementations for smart pointers (`Box`, `Rc`, `Arc`, `RefCell`)
- Tuple support up to 12 elements

### 4. Mark-Sweep Collection Algorithm

**Status: Complete**

- Minor GC (young generation only)
- Major GC (full heap)
- Two-phase sweep (Phase 1: Drop, Phase 2: Reclaim) to prevent use-after-free
- Generational support with promotion from Gen 0 to Gen 1
- Write barriers via dirty bitmap for remembered set

### 5. Multi-threaded GC Coordination

**Status: Complete**

- `ThreadControlBlock` for per-thread GC state
- `ThreadRegistry` for tracking all GC-enabled threads
- Cooperative safepoint handshake protocol
- `GC_REQUESTED` atomic flag for stop-the-world coordination
- Orphan page handling for terminated threads

### 6. Weak References

**Status: Complete**

- `Weak<T>` type with `upgrade()` method
- Weak count tracking in `GcBox`
- Objects with weak refs: value dropped but allocation retained

### 7. GcCell for Interior Mutability

**Status: Complete**

- `GcCell<T>` similar to `RefCell` with write barrier integration
- Automatic dirty bit setting on mutable borrow

### 8. GC Metrics

**Status: Basic Implementation Complete**

- `GcMetrics` struct with duration, bytes reclaimed/surviving, etc.
- `last_gc_metrics()` API
- Collection type tracking (Minor/Major)

---

## ❌ Not Implemented (Excluding Parallel Marking)

### 1. Incremental / Concurrent Marking

**Priority: High**

The design document emphasizes low-latency GC via incremental marking:

> "V8 的優勢： 它在 Marking 和 Sweeping 階段極度積極地使用 Concurrency... 這意味著你的 Server 很少會出現長達幾百毫秒的停頓。"

Current implementation is **Stop-the-World only**. No incremental progress during mutator execution.

**Missing:**
- Tri-color marking abstraction
- Write barriers for concurrent marking (Dijkstra's invariant)
- Incremental work scheduling

### 2. Self-referential Cycles (`new_cyclic`)

**Priority: Medium**

The `Gc::new_cyclic` function exists but is explicitly non-functional:

```rust
// From ptr.rs - FIXME comment
// FIXME: Self-referential cycle support is not implemented.
// The rehydrate_self_refs function is essentially a no-op.
```

**Impact:** Users cannot create self-referential structures without manual workarounds.

### 3. Cross-platform Stack Bounds

**Priority: Medium**

Stack bounds detection is only implemented for Linux:

```rust
// From stack.rs
#[cfg(all(not(target_os = "linux"), not(miri)))]
pub fn get_stack_bounds() -> StackBounds {
    unimplemented!("Stack bounds retrieval only implemented for Linux")
}
```

**Missing:**
- macOS implementation (via `pthread_get_stackaddr_np`)
- Windows implementation (via `GetCurrentThreadStackLimits`)
- Other Unix variants

### 4. Interior Pointer Support for Small Objects

**Priority: Low**

Currently, small objects require pointers to point to the **start** of the object:

```rust
// From heap.rs - find_gc_box_from_ptr
} else if offset_to_use % block_size_to_use != 0 {
    // For small objects, we still require them to point to the start
    return None;
}
```

Large objects support interior pointers; small objects do not.

### 5. Bloom Filter for Fast Pointer Filtering

**Priority: Low (Optimization)**

Design document suggested:

> "是否可以使用 Bloom Filter 來快速過濾掉不在我們 Heap 中的指針？"

Current implementation uses:
- Range check (`heap.is_in_range()`)
- `HashSet<usize>` for small page addresses

A Bloom filter could reduce false-positive pointer scans.

### 6. Adaptive GC Heuristics

**Priority: Low**

The `default_collect_condition` is simplistic:

```rust
pub const fn default_collect_condition(info: &CollectInfo) -> bool {
    info.n_gcs_dropped > info.n_gcs_existing || info.young_size > 1024 * 1024
}
```

**Missing:**
- Adaptive threshold based on allocation rate
- GC pause time targeting
- Memory pressure detection

### 7. Finalize Trait (Custom Drop Ordering)

**Priority: Low**

Design mentioned:

> "對於循環中的物件，我們可能需要提供一個 Finalize trait，而不是依賴 Rust 原生的 Drop"

Current implementation uses standard `Drop` trait. Finalization order in cycles is undefined.

---

## ⚠️ Partial Implementations

### 1. Miri Compatibility

Stack scanning returns dummy results under Miri. Tests use `register_test_root()` as a workaround:

```rust
#[cfg(miri)]
pub fn get_stack_bounds() -> StackBounds {
    StackBounds { bottom: 0, top: 0 }  // No scanning under Miri
}
```

This is acceptable but limits Miri's ability to verify full GC behavior.

### 2. Generational Promotion Policy

Current policy promotes after **first survival**:

```rust
if has_survivors {
    (*header).generation = 1;  // Immediate promotion
}
```

More sophisticated tenure thresholds (e.g., survive N collections) could reduce promotion of short-lived objects.

---

## Comparison with Design Goals

| Design Goal | Status | Notes |
|-------------|--------|-------|
| BiBOP Memory Layout | ✅ Complete | Full implementation with 8 size classes |
| Conservative Stack Scanning | ✅ Complete | Linux only, Miri-compatible fallback |
| Trace Trait & Derive Macro | ✅ Complete | Comprehensive std type coverage |
| Non-moving GC | ✅ Complete | Address stability preserved |
| Write Barriers | ✅ Complete | Dirty bitmap implementation |
| Generational GC | ✅ Complete | Young/Old generations |
| Multi-threaded Safety | ✅ Complete | Safepoint handshake protocol |
| Parallel Marking | ❌ Not Done | (Excluded from analysis) |
| Incremental Marking | ❌ Not Done | Stop-the-world only |
| Concurrent Sweeping | ❌ Not Done | Single-threaded sweep |
| Cross-platform | ⚠️ Partial | Linux only |
| Self-referential Cycles | ❌ Not Done | `new_cyclic` non-functional |

---

## Recommendations for Future Work

1. **Incremental Marking** - Most impactful for latency-sensitive applications
2. **Cross-platform Support** - Required for wider adoption
3. **`new_cyclic` Implementation** - Important for graph-like data structures
4. **Parallel Sweeping** - Simpler than parallel marking, good latency win
5. **Adaptive Heuristics** - Better out-of-box performance tuning
