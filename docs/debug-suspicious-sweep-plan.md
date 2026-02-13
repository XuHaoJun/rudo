# debug-suspicious-sweep Implementation Plan

## Overview

This feature detects when young generation objects are incorrectly swept due to `Vec<Gc<T>>` being stored outside GC-managed memory. It provides helpful panic messages pointing users to the correct solution.

**Feature Flags**: `debug-suspicious-sweep`, `paranoid-sweep`

**Priority**: P0 (Critical)

---

## Background

When `Vec<Gc<T>>` is stored in a standard Rust allocation (not GC-managed), the GC cannot find and mark the contained `Gc<T>` pointers during collection:

1. Stack scanning doesn't find the Vec's contents
2. Write barriers don't mark the Vec's buffer as dirty
3. Gc pointers inside the Vec are not marked as reachable
4. Objects are incorrectly swept and their memory is reused

This leads to memory corruption with dangling pointers.

**Root Cause**: The Vec is stored in a non-GC-managed allocation (like `RefCell<Vec<Gc<Component>>>`), so when GC runs:
- The Vec itself is not traced (it's not a GC object)
- The contained `Gc<T>` elements are never discovered
- These objects are incorrectly collected

---

## Design Decisions

### Ring Buffer Size

**Decision**: 1024 entries (configurable)

**Rationale** (from R. Kent Dybvig):
- You don't need to track *all* objects — just enough to catch the bug
- 1024 objects × ~16 bytes each = 16KB — negligible overhead
- Young objects die fast — most entries will be recycled within a few collections
- Make configurable via environment variable or compile-time constant

### Suspicious Threshold

**Decision**: ≤2 GC cycles

**Rationale** (from R. Kent Dybvig):
- Objects created in the current or previous GC cycle are suspicious
- Formula: `is_suspicious = (current_gc_id - allocation_gc_id) <= 2`
- A brand-new object being swept is clearly wrong
- An object that survived one minor GC but wasn't promoted yet is also suspicious
- After promotion, objects are fair game

---

## Implementation Phases

### Phase 1: Feature Flags & Configuration

#### 1.1 Cargo.toml

```toml
[features]
default = ["lazy-sweep", "derive"]
# ... existing features ...
debug-suspicious-sweep = []      # Detection enabled in debug builds
paranoid-sweep = ["debug-suspicious-sweep"]  # More aggressive detection
```

---

### Phase 2: Young Object History Tracking

#### 2.1 New Module: `src/gc/young_object_history.rs`

```rust
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

const DEFAULT_HISTORY_SIZE: usize = 1024;

pub struct YoungObjectHistory {
    /// Ring buffer of recently allocated young objects
    records: Vec<YoungObjectRecord>,
    /// Current write index (wraps around)
    write_idx: AtomicUsize,
    /// GC cycle ID when recording started
    initial_gc_id: AtomicU64,
    /// Maximum history size
    max_size: usize,
}

struct YoungObjectRecord {
    ptr: *const u8,
    gc_id: u64,
    size: usize,
}
```

#### 2.2 Integration Points

| Location | Change |
|----------|--------|
| `src/ptr.rs:753` | After `crate::gc::notify_created_gc()`, call `young_object_history::record(ptr)` |
| `src/gc/gc.rs:188` | Extend `notify_created_gc()` to record allocation |

---

### Phase 3: Sweep Phase Detection Hooks

#### 3.1 Detection in `sweep_phase1_finalize()` (gc/gc.rs:2023)

```rust
fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {
    // ... existing code ...
    
    for i in 0..obj_count {
        if (*header).is_marked(i) {
            // Object is reachable - clear mark
            (*header).clear_mark(i);
        } else if (*header).is_allocated(i) {
            // CHECK: Is this a suspicious sweep?
            #[cfg(feature = "debug-suspicious-sweep")]
            if (*header).generation == 0 && !only_young {
                // This is a young object being swept during OLD gen collection
                check_suspicious_sweep(obj_ptr, &(*header).generation);
            }
            // ... rest of cleanup ...
        }
    }
}
```

#### 3.2 The Detection Logic

```rust
fn check_suspicious_sweep(obj_ptr: *const u8, expected_gen: &u8) {
    // 1. Check if this pointer is in young object history
    // 2. If found and was created recently (within N GC cycles), it's SUSPICIOUS
    // 3. Trigger panic with helpful message
}
```

---

### Phase 4: Helpful Panic Message

```
Thread 'main' panicked at 'rudo-gc detected suspicious GC behavior:

A young generation object (ptr=0x600000000668) was not marked but is being swept.
This typically indicates Vec<Gc<T>> was used without Gc<Vec<Gc<T>>>.

Solution:
  Change: let items: RefCell<Vec<Gc<T>>> = ...
  To:     let items: Gc<RefCell<Vec<Gc<T>>>> = Gc::new(RefCell::new(Vec::new()));

For more information, see: crates/rudo-gc/docs/vec-gc-usage.md

This check only runs in debug builds. Enable 'debug-suspicious-sweep' feature for release builds.
'
```

---

### Phase 5: Public API

#### 5.1 New Functions (src/lib.rs)

```rust
#[cfg(feature = "debug-suspicious-sweep")]
pub fn set_suspicious_sweep_detection(enabled: bool) {
    // Toggle detection at runtime
}

#[cfg(feature = "debug-suspicious-sweep")]
pub fn get_suspicious_sweep_stats() -> SuspiciousSweepStats {
    // Return detection statistics
}
```

---

### Phase 6: Integration Tests

**Test cases to add**:

1. `Vec<Gc<T>>` in RefCell → should panic with helpful message
2. `Gc<Vec<Gc<T>>>` → should work correctly (baseline)
3. Multiple iterations → ensure detection works consistently
4. Edge case: object created, GC runs, object survives, GC runs again → should NOT panic after promotion

Add a **disabled test** (behind feature flag) that verifies the fix actually works.

---

## Implementation Order

| Step | File | Description |
|------|------|-------------|
| 1 | `Cargo.toml` | Add feature flags |
| 2 | `src/gc/young_object_history.rs` | New module with ring buffer |
| 3 | `src/gc/mod.rs` | Export new module |
| 4 | `src/gc/gc.rs` | Extend `notify_created_gc()` |
| 5 | `src/gc/gc.rs` | Add detection in `sweep_phase1_finalize()` |
| 6 | `src/lib.rs` | Add public API |
| 7 | `tests/` | Add integration tests |
| 8 | `docs/vec-gc-usage.md` | Documentation |

---

## Files to Modify

- `crates/rudo-gc/Cargo.toml` — Add features
- `crates/rudo-gc/src/gc/young_object_history.rs` — New file
- `crates/rudo-gc/src/gc/mod.rs` — Export module  
- `crates/rudo-gc/src/gc/gc.rs` — Add hooks
- `crates/rudo-gc/src/ptr.rs` — Hook into allocation
- `crates/rudo-gc/src/lib.rs` — Public API
- `crates/rudo-gc/docs/vec-gc-usage.md` — New documentation
- `crates/rudo-gc/tests/` — Integration tests

---

## Key Design Principles

1. **Ring buffer**: O(1) insert, fixed memory overhead (~16KB for 1024 entries)
2. **GC ID tracking**: Use existing `next_gc_id()` from tracing module
3. **Conditional compilation**: Zero overhead when feature disabled
4. **Thread-local storage**: Use thread-local for per-thread tracking, aggregate globally
5. **Runtime toggle**: Allow enabling/disabling at runtime for production debugging

---

## Related Documentation

- `README.md` (lines 789-908) - Vec<Gc<T>> corruption problem description
- `tests/gccell_vec_corruption_regression.rs` - Existing regression test
- `tests/vec_gc_corruption_minimal.rs` - Minimal reproduction
- `tests/vec_gc_corruption_bug_report.md` - Bug analysis

---

## References

- R. Kent Dybvig's recommendations on threshold and buffer size
- Chez Scheme's fault injection testing approach
- V8's similar detection mechanisms for debugging
