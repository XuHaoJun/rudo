# Quickstart: Implementing Lazy Sweep

## Overview

This guide provides implementation steps for the lazy sweep feature. The implementation follows the research decisions documented in `research.md` and uses the data model defined in `data-model.md`.

## Implementation Phases

### Phase 1: Infrastructure (Day 1)

**Goal**: Add data structures and flags

**Tasks**:

1. **Add sweep flags to heap.rs** (~line 426)
   ```rust
   pub const PAGE_FLAG_NEEDS_SWEEP: u8 = 0x04;
   pub const PAGE_FLAG_ALL_DEAD: u8 = 0x08;
   ```

2. **Add helper methods to PageHeader**
   ```rust
   pub const fn needs_sweep(&self) -> bool { ... }
   pub fn set_needs_sweep(&mut self) { ... }
   pub fn clear_needs_sweep(&mut self) { ... }
   pub fn all_dead(&self) -> bool { ... }
   pub fn set_all_dead(&mut self) { ... }
   pub fn clear_all_dead(&mut self) { ... }
   ```

3. **Add dead_count field to PageHeader**
   - Replace `_padding: [u8; 2]` with `dead_count: Cell<u16>`
   - No memory overhead (uses existing padding)

4. **Update Cargo.toml**
   ```toml
   [features]
   default = ["lazy-sweep", "derive"]
   lazy-sweep = []
   derive = []
   ```

---

### Phase 2: Core Implementation (Day 2)

**Goal**: Implement lazy sweep functions

**Tasks**:

1. **Add lazy sweep functions to gc.rs**
   - `lazy_sweep_page()` - Main sweep function, processes up to 16 objects
   - `lazy_sweep_page_all_dead()` - Fast path for entirely-dead pages
   - `sweep_pending()` - Public API, sweeps multiple pages
   - `pending_sweep_count()` - Returns pages needing sweep

2. **Key algorithm**:
   ```rust
   // For each object in page (up to batch size):
   // 1. Check if allocated and not marked (dead object)
   // 2. If weak refs exist: drop value, keep allocation
   // 3. If no weak refs: reclaim, add to free list
   // 4. Clear mark bits for surviving objects
   // 5. If all dead: set all_dead flag
   ```

3. **Weak reference handling**:
   - Check `weak_count()` on each dead object
   - If > 0: only drop value, keep allocation (dead flag set)
   - If == 0: fully reclaim (add to free list)

---

### Phase 3: Integration (Day 3)

**Goal**: Connect lazy sweep to existing GC infrastructure

**Tasks**:

1. **Modify mark phase** (in `perform_multi_threaded_collect`)
   - After marking: iterate all pages
   - Set `needs_sweep` flag on pages with allocated objects
   - Do NOT sweep - return immediately

2. **Integrate with allocation path** (in `heap.rs`)
   - Add `alloc_from_pending_sweep()` helper
   - Call before `alloc_slow()` (new page allocation)
   - Scan pages for one needing sweep, try to reclaim

3. **Add safepoint trigger**
   - In `check_safepoint()`: add sweep work trigger
   - Use adaptive frequency based on pending count

4. **Add public API** (in `lib.rs`)
   - `sweep_pending(num_pages) -> usize`
   - `pending_sweep_pages() -> usize`

---

### Phase 4: Testing & Tuning (Day 4)

**Goal**: Verify correctness and performance

**Tasks**:

1. **Write integration tests** (`tests/lazy_sweep.rs`)
   - `test_lazy_sweep_frees_dead_objects`
   - `test_lazy_sweep_preserves_live_objects`
   - `test_lazy_sweep_all_dead_optimization`
   - `test_lazy_sweep_weak_refs`
   - `test_lazy_sweep_minor_gc`
   - `test_lazy_sweep_major_gc`
   - `test_lazy_sweep_large_object_still_eager`
   - `test_lazy_sweep_orphan_still_eager`

2. **Run existing tests**
   - `./test.sh` - All tests pass
   - `./clippy.sh` - No warnings
   - `./miri-test.sh` - Unsafe code passes

3. **Run benchmarks** (`tests/benchmarks/sweep_comparison.rs`)
   - Compare eager vs lazy pause times
   - Verify O(1) amortized allocation

---

## Key Files Modified

| File | Changes |
|------|---------|
| `crates/rudo-gc/src/heap.rs` | Add flags, dead_count, modify alloc |
| `crates/rudo-gc/src/gc/gc.rs` | Modify mark phase, add lazy sweep functions |
| `crates/rudo-gc/src/lib.rs` | Add public API |
| `crates/rudo-gc/Cargo.toml` | Add lazy-sweep feature |
| `crates/rudo-gc/tests/lazy_sweep.rs` | New test file |
| `crates/rudo-gc/tests/benchmarks/sweep_comparison.rs` | New benchmark file |

---

## Verification Checklist

- [ ] `./clippy.sh` passes with zero warnings
- [ ] `cargo fmt --all` produces no changes
- [ ] `./test.sh` passes all tests (including ignored)
- [ ] `./miri-test.sh` passes for unsafe code
- [ ] Benchmarks show pause time reduction (O(pages+objects) → O(1))
- [ ] Heap memory bounded under allocation/deallocation workload
- [ ] Large objects still reclaimed promptly (eager)
- [ ] Weak references work correctly with lazy sweep
- [ ] Feature flag disables lazy sweep correctly
