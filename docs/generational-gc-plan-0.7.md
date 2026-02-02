# rudo-gc Fully Generational GC Implementation Plan

**Version**: 0.6 → 0.7
**Date**: 2026-02-02
**Status**: Approved for Implementation
**Generation Model**: 2-generation (young/old)
**Card Size**: 512 bytes (Chez Scheme default)
**Activation**: Default-on (no feature flag)
**Team**: 3 AI Agents

---

## 1. Executive Summary

**Goal**: Complete the partial generational GC implementation to achieve fast minor collections that target the short-lived objects common in game/UI workloads.

**Target Improvement**: 70-90% reduction in minor GC pause times through efficient remembered set scanning instead of full heap scanning.

**Approach**: Implement a card-table-based remembered set inspired by Chez Scheme, integrated with the existing parallel marking infrastructure.

**Scope**: 3-week implementation with 3 agents, followed by 1 week of testing and polish.

---

## 2. Current State Analysis

### 2.1 Existing Partial Implementation

The following generational GC components are already implemented:

| Component | Status | Location | Notes |
|-----------|--------|----------|-------|
| Generation field in PageHeader | ✅ Done | `src/heap.rs:539-540` | 0=young, 1=old |
| Young/old allocation counters | ✅ Done | `src/heap.rs:1224-1225` | In LocalHeap |
| Write barrier (dirty bits) | ✅ Done | `src/cell.rs:76-125` | GcCell::write_barrier |
| Minor collection orchestration | ✅ Done | `src/gc/gc.rs:1131-1141` | collect_minor functions |
| VisitorKind::Minor filtering | ✅ Done | `src/trace.rs:84-89` | Skips old gen objects |
| Promotion logic | ✅ Done | `src/gc/gc.rs:1145-1178` | promote_young_pages |

### 2.2 Missing Components

| Component | Priority | Description |
|-----------|----------|-------------|
| **Remembered Set** | P0 | Track old→young pointers explicitly |
| **Card Table** | P0 | Efficient granularity for dirty tracking |
| **Minor Root Scanning** | P0 | Use RemSet instead of dirty pages |
| **Write Barrier Integration** | P0 | Add to remembered set on mutation |
| **Parallel Minor GC** | P1 | Distribute RemSet across workers |

### 2.3 Architecture Comparison

**Before (0.6)**:
```
Minor GC Flow:
1. Mark explicit roots (stack, registers)
2. Scan ALL dirty pages (entire old generation if dirty)
3. For each dirty object, mark if not already marked
4. Trace from marked objects (stop at old gen)
5. Sweep dead objects
6. Promote all survivors
```

**After (0.7)**:
```
Minor GC Flow:
1. Mark explicit roots (stack, registers)
2. Iterate remembered set entries (only actual old→young refs)
3. For each remembered reference, mark the young object
4. Trace from marked objects (stop at old gen)
5. Sweep dead objects
6. Promote all survivors
7. Clear remembered set
```

**Improvement**: Step 2 goes from O(old_gen_size) to O(remembered_refs), typically 10-100x fewer objects.

---

## 3. Architecture Design

### 3.1 System Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Global Remembered Set                      │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  Card Table (512-byte cards)                            ││
│  │  ┌─────┬─────┬─────┬─────┬─────┬─────┐                  ││
│  │  │Dirty│Dirty│Clean│Dirty│Clean│... │                  ││
│  │  └─────┴─────┴─────┴─────┴─────┴─────┘                  ││
│  │                                                         ││
│  │  Per-Card Dirty Byte = youngest generation pointed to   ││
│  │  (0=clean, 1=young generation reference)               ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Minor Collection Flow                      │
│  1. Scan Remembered Set (old→young references)              │
│  2. Mark referenced young objects                           │
│  3. Mark explicit roots (stack, registers)                  │
│  4. Trace from marked objects (young only)                  │
│  5. Sweep dead young objects                                │
│  6. Promote survivors to old generation                     │
│  7. Clear remembered set                                    │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 Data Structures

#### 3.2.1 PageHeader Modifications

**File**: `src/heap.rs`

```rust
// In PageHeader struct, add after dirty_bitmap:
/// Card table for generational GC - 512 bytes per card
pub card_table: Vec<AtomicU8>,  // 0=clean, 1=young generation pointed to
/// Card size (fixed at 512 bytes for 0.7)
pub const CARD_SIZE: usize = 512;
/// Number of cards per page (4KB / 512 = 8)
pub const CARDS_PER_PAGE: usize = PAGE_SIZE / Self::CARD_SIZE;
```

**Initialization**:
- Modified `PageHeader::new()` to initialize card_table
- Updated `page_header_size()` to account for card table overhead
- Add `card_index(ptr: *const u8) -> usize` helper method

#### 3.2.2 RememberedSet Structure

**New File**: `src/gc/remembered_set.rs`

```rust
use std::sync::atomic::{AtomicUsize, AtomicU8, Ordering};
use std::ptr::NonNull;
use crate::heap::PageHeader;

/// Card table entry - stores youngest generation pointed to
/// 0 = clean (no young references)
/// 1 = has young generation reference
pub type CardEntry = AtomicU8;

pub struct RememberedSet {
    /// Per-page card tables (indexed by page address)
    pages: parking_lot::RwLock<HashMap<NonNull<PageHeader>, PageRememberedSet>>,
    /// Statistics
    total_pages: AtomicUsize,
    dirty_cards: AtomicUsize,
}

struct PageRememberedSet {
    cards: NonNull<CardEntry>,  // Aligned CARDS_PER_PAGE entries
    card_count: usize,
}

impl RememberedSet {
    /// Create a new empty remembered set
    pub fn new() -> Self { ... }

    /// Mark a card as dirty (called by write barrier)
    pub fn mark_dirty(&self, page: NonNull<PageHeader>, card_idx: usize) { ... }

    /// Iterate over all dirty cards in a page (for minor GC)
    pub fn iter_dirty_cards(&self, page: NonNull<PageHeader>) -> impl Iterator<Item = *const GcBox<()>> { ... }

    /// Clear all cards in a page (called after minor GC)
    pub fn clear_page(&self, page: NonNull<PageHeader>) { ... }

    /// Clear all pages (called after minor collection)
    pub fn clear_all(&self) { ... }

    /// Collect statistics
    pub fn stats(&self) -> RemSetStats { ... }
}

pub struct RemSetStats {
    pub total_pages: usize,
    pub dirty_cards: usize,
    pub memory_overhead: usize,
}
```

#### 3.2.3 LocalHeap Modifications

**File**: `src/heap.rs`

```rust
// In LocalHeap struct, add:
/// Remembered set for cross-generational references
pub remembered_set: RwLock<RememberedSet>,
/// Minor collection count for statistics
pub minor_gc_count: AtomicUsize,
/// Objects promoted in current cycle
pub objects_promoted: AtomicUsize,
```

### 3.3 Write Barrier Design

#### 3.3.1 Enhanced GcCell Write Barrier

**File**: `src/cell.rs`

**Current Implementation**:
```rust
fn write_barrier(&self) {
    let header = self.ptr.header();
    if header.generation > 0 {  // Old generation
        let idx = self.ptr.object_index();
        header.set_dirty(idx);  // Sets per-object dirty bit
    }
}
```

**New Implementation**:
```rust
fn write_barrier(&self, new_value: &T) {
    let header = self.ptr.header();

    // Check if we're an old object writing to a young reference
    if header.generation > 0 {
        // Get the new value's generation
        let target_header = new_value.ptr.header();
        if target_header.generation == 0 {
            // Old → Young reference! Record in remembered set
            let tcb = current_thread_control_block();
            let heap = tcb.local_heap();
            heap.remembered_set.write().mark_dirty(
                header.page(),
                card_index(new_value.ptr.as_ptr())
            );
        }
    }

    // Also set dirty bit for other purposes (compatibility)
    let idx = self.ptr.object_index();
    header.set_dirty(idx);
}
```

#### 3.3.2 Gc<T> Operations

**File**: `src/ptr.rs` or `src/gc/gc.rs`

Add write barrier to `Gc<T>` clone and assignment operations:

```rust
impl<T> Gc<T> {
    /// Clone with write barrier (if interior mutability needed)
    pub fn try_clone(&self) -> Option<Gc<T>>
    where T: Trace {
        // Call write barrier before clone
        self.write_barrier();
        // ... existing clone logic
    }

    /// Internal write barrier call
    fn write_barrier(&self) {
        // Record in remembered set if applicable
    }
}
```

### 3.4 Minor Collection Design

#### 3.4.1 Mark Minor Roots with Remembered Set

**File**: `src/gc/gc.rs`

**Current** (`mark_minor_roots_multi`):
```rust
// Line 775-790: Scans ALL objects in dirty pages
for i in 0..obj_count {
    if (*header).is_dirty(i) {
        // Mark this object...
    }
}
```

**New Implementation**:
```rust
fn mark_minor_roots_multi(heap: &LocalHeap, visitor: &mut GcVisitor) {
    // 1. Mark explicit roots (stack, registers)
    mark_minor_roots(heap, visitor);

    // 2. Mark from remembered set (old→young references)
    let rem_set = heap.remembered_set.read();

    // Iterate over all pages with dirty cards
    rem_set.iter_pages_with_dirty_cards(|page| {
        // For each dirty card, mark the objects
        rem_set.iter_dirty_cards(page).for_each(|obj_ptr| {
            visitor.visit_gcptr(obj_ptr.as_gcptr());
        });
    });
}
```

#### 3.4.2 Parallel Minor GC Distribution

**File**: `src/gc/gc.rs`

Modify `collect_minor_multi` to distribute remembered set entries across workers:

```rust
fn collect_minor_multi(heaps: &[&mut LocalHeap]) -> usize {
    // 1. Gather all remembered set entries from all threads
    let mut all_remembered: Vec<*const GcBox<()>> = Vec::new();
    for heap in heaps {
        let rem_set = heap.remembered_set.read();
        rem_set.collect_entries(&mut all_remembered);
    }

    // 2. Distribute across worker queues (round-robin)
    distribute_work_across_workers(&all_remembered);

    // 3. Workers mark (already filters by VisitorKind::Minor)
    mark_minor_roots_parallel(heaps);

    // 4. Sweep young pages
    let reclaimed = sweep_segment_pages(heaps[0], true);

    // 5. Promote survivors
    promote_young_pages(heaps[0]);

    // 6. Clear remembered set
    for heap in heaps {
        heap.remembered_set.write().clear_all();
    }

    reclaimed
}
```

---

## 4. Implementation Phases

### Phase 1: Data Structure Foundation (Agent 1)

**Duration**: 2-3 days (Week 1, Mon-Wed)

**Tasks**:
1. Add card table to `PageHeader` in `src/heap.rs`
2. Create `RememberedSet` struct in `src/gc/remembered_set.rs`
3. Create card table utilities in `src/gc/card_table.rs`
4. Create `GcConfig` in `src/gc/config.rs`
5. Update `LocalHeap` to include `remembered_set` field
6. Write unit tests for data structures

**Deliverables**:
- `src/gc/remembered_set.rs` - Core remembered set implementation
- `src/gc/card_table.rs` - Card table utilities
- `src/gc/config.rs` - GC configuration
- `tests/generation_card_table.rs` - Card table unit tests
- Modified `src/heap.rs` - PageHeader and LocalHeap updates

**Key Functions to Implement**:
```rust
// In src/gc/remembered_set.rs
impl RememberedSet {
    pub fn new() -> Self
    pub fn mark_dirty(&self, page: NonNull<PageHeader>, card_idx: usize)
    pub fn iter_dirty_cards(&self, page: NonNull<PageHeader>) -> impl Iterator
    pub fn clear_page(&self, page: NonNull<PageHeader>)
    pub fn clear_all(&self)
    pub fn stats(&self) -> RemSetStats
}

// In src/heap.rs (PageHeader)
impl PageHeader {
    pub fn card_index(ptr: *const u8) -> usize
    pub fn is_card_dirty(&self, card_idx: usize) -> bool
    pub fn set_card_dirty(&self, card_idx: usize)
    pub fn clear_card(&self, card_idx: usize)
    pub fn clear_all_cards(&self)
}
```

---

### Phase 2: Write Barrier Integration (Agent 2)

**Duration**: 2-3 days (Week 1, Thu - Week 2, Tue)

**Dependencies**: Phase 1 (for RememberedSet API)

**Tasks**:
1. Enhance `GcCell::write_barrier` to record old→young references
2. Add write barrier to `Gc<T>` clone and assignment operations
3. Update derive macro to generate barrier-aware Trace implementations
4. Write tests for write barrier correctness

**Deliverables**:
- Modified `src/cell.rs` - Enhanced write barrier
- Modified `src/ptr.rs` or `src/gc/gc.rs` - Gc<T> barrier calls
- Modified `rudo-gc-derive/` - Trace impl updates
- `tests/generation_write_barrier.rs` - Write barrier tests

**Key Functions to Implement**:
```rust
// In src/cell.rs (GcCell)
impl<T> GcCell<T> {
    fn write_barrier(&self, new_value: &T) {
        let header = self.ptr.header();

        // Old → Young reference?
        if header.generation > 0 {
            let target_header = new_value.ptr.header();
            if target_header.generation == 0 {
                // Record in remembered set
                let tcb = current_thread_control_block();
                let heap = tcb.local_heap();
                heap.remembered_set.write().mark_dirty(
                    header.page(),
                    card_index(new_value.ptr.as_ptr())
                );
            }
        }

        // Set dirty bit for compatibility
        let idx = self.ptr.object_index();
        header.set_dirty(idx);
    }
}
```

---

### Phase 3: Minor Collection Implementation (Agent 3)

**Duration**: 3-4 days (Week 2, Mon-Wed)

**Dependencies**: Phase 1 (data structures), Phase 2 (write barrier)

**Tasks**:
1. Replace dirty page scanning with remembered set in `mark_minor_roots_multi`
2. Implement parallel distribution of remembered set entries
3. Integrate with existing parallel marking infrastructure
4. Add generational metrics to `GcMetrics`
5. Write integration tests

**Deliverables**:
- Modified `src/gc/gc.rs` - Minor collection with RemSet
- Modified `src/gc/marker.rs` - Parallel marking integration
- Modified `src/metrics.rs` - Generational metrics
- Modified `src/lib.rs` - Config integration
- `tests/generation_parallel.rs` - Parallel minor GC tests
- `tests/generation_minor_collection.rs` - Minor collection tests

**Key Functions to Implement**:
```rust
// In src/gc/gc.rs
fn mark_minor_roots_multi(heap: &LocalHeap, visitor: &mut GcVisitor) {
    // 1. Mark explicit roots (stack, registers)
    mark_minor_roots(heap, visitor);

    // 2. Mark from remembered set (old→young references)
    let rem_set = heap.remembered_set.read();
    rem_set.iter_pages_with_dirty_cards(|page| {
        rem_set.iter_dirty_cards(page).for_each(|obj_ptr| {
            visitor.visit_gcptr(obj_ptr.as_gcptr());
        });
    });
}

fn collect_minor_multi(heaps: &[&mut LocalHeap]) -> usize {
    // 1. Gather all remembered set entries
    let mut all_remembered: Vec<*const GcBox<()>> = Vec::new();
    for heap in heaps {
        let rem_set = heap.remembered_set.read();
        rem_set.collect_entries(&mut all_remembered);
    }

    // 2. Distribute across worker queues
    distribute_work_across_workers(&all_remembered);

    // 3. Workers mark
    mark_minor_roots_parallel(heaps);

    // 4. Sweep young pages
    let reclaimed = sweep_segment_pages(heaps[0], true);

    // 5. Promote survivors
    promote_young_pages(heaps[0]);

    // 6. Clear remembered set
    for heap in heaps {
        heap.remembered_set.write().clear_all();
    }

    reclaimed
}
```

---

### Phase 4: Testing and Integration (All Agents)

**Duration**: 3-4 days (Week 2, Thu - Week 3, Wed)

**Tasks**:
1. Run full test suite (./test.sh)
2. Run clippy (./clippy.sh)
3. Run Miri tests
4. Fix any integration issues
5. Performance optimization
6. Update README with generational GC documentation

**Activities**:
- Address any test failures
- Profile performance and optimize hot paths
- Update documentation
- Run benchmarks

---

## 5. File Changes Summary

### 5.1 New Files (6)

| File | Purpose |
|------|---------|
| `src/gc/remembered_set.rs` | Core remembered set implementation |
| `src/gc/card_table.rs` | Card table utilities |
| `src/gc/config.rs` | GC configuration |
| `tests/generation_card_table.rs` | Card table unit tests |
| `tests/generation_remembered_set.rs` | Remembered set unit tests |
| `tests/generation_write_barrier.rs` | Write barrier tests |

### 5.2 Modified Files (7)

| File | Changes |
|------|---------|
| `src/heap.rs` | PageHeader (card table), LocalHeap (remembered_set field) |
| `src/cell.rs` | Enhanced write barrier |
| `src/ptr.rs` | Gc<T> barrier calls |
| `src/gc/gc.rs` | Minor collection algorithms |
| `src/gc/marker.rs` | Parallel marking integration |
| `src/metrics.rs` | Generational metrics |
| `src/lib.rs` | Config integration |

### 5.3 Test Files to Create (5+)

| File | Coverage |
|------|----------|
| `tests/generation_card_table.rs` | Card allocation, indexing, dirty tracking |
| `tests/generation_remembered_set.rs` | Add, iterate, clear operations |
| `tests/generation_write_barrier.rs` | Old→young reference recording |
| `tests/generation_minor_collection.rs` | Full minor collection flow |
| `tests/generation_parallel.rs` | Multi-threaded minor GC |
| `tests/generation_cycle.rs` | Cycles across generations |

### 5.4 Benchmark Files to Create (2+)

| File | Metric |
|------|--------|
| `benchmarks/generation_minor_gc.rs` | Minor GC pause time |
| `benchmarks/generation_throughput.rs` | Allocation throughput |

---

## 6. Testing Strategy

### 6.1 Unit Tests

All new data structures and algorithms must have unit tests with >90% coverage:

```rust
// Example test structure in tests/generation_card_table.rs
#[test]
fn test_card_index_calculation() {
    let page = allocate_test_page();
    let obj_ptr = page.as_ptr().add(256);  // Middle of page
    let card_idx = card_index(obj_ptr);
    assert_eq!(card_idx, 0);  // First card (0-511 bytes)
}

#[test]
fn test_card_dirty_tracking() {
    let page = allocate_test_page();
    assert!(!page.is_card_dirty(0));

    page.set_card_dirty(0);
    assert!(page.is_card_dirty(0));

    page.clear_card(0);
    assert!(!page.is_card_dirty(0));
}
```

### 6.2 Integration Tests

Test complete workflows with generational GC:

```rust
// Example in tests/generation_minor_collection.rs
#[test]
fn test_minor_collection_with_remembered_set() {
    let mut heap = LocalHeap::new();

    // Allocate old object
    let old_obj = Gc::new(OldStruct { young_ref: None });

    // Allocate young object
    let young_obj = Gc::new(YoungStruct { value: 42 });

    // Create old→young reference (should be remembered)
    *old_obj.young_ref.borrow_mut() = Some(young_obj);

    // Trigger minor collection
    heap.collect_minor();

    // Verify young object was marked and preserved
    assert!(!young_obj.is_dead());
}
```

### 6.3 Loom Tests

Test concurrent access patterns:

```rust
// In tests/loom_generational.rs (add to existing loom tests)
#[test]
#[cfg_attr(miri, ignore)]
fn test_remembered_set_concurrent_write_barrier() {
    loom::model(|| {
        let heap = LocalHeap::new();
        let old_obj = Gc::new_in(OldStruct::default(), &heap);
        let young_obj = Gc::new_in(YoungStruct::default(), &heap);

        // Concurrent write barrier calls
        let (tx, rx) = crossbeam::channel::bounded(2);
        std::thread::spawn({
            let tx = tx.clone();
            let mut ref_cell = old_obj.young_ref.borrow_mut();
            move || {
                *ref_cell = Some(young_obj);
                tx.send(()).unwrap();
            }
        });

        // Simulate minor collection
        std::thread::spawn(|| {
            heap.collect_minor();
        }).join().unwrap();
    });
}
```

### 6.4 Performance Benchmarks

Create benchmarks to verify performance improvement:

```rust
// In benchmarks/generation_minor_gc.rs
fn bench_minor_gc_pause_time(c: &mut Criterion) {
    c.bench_function("minor_gc_pause_10mb", |b| {
        b.iter(|| {
            // Allocate 10MB of young objects
            // Create old→young references
            // Trigger minor collection
            // Measure pause time
        });
    });
}

fn bench_minor_gc_improvement(c: &mut Criterion) {
    // Compare 0.6 vs 0.7 minor GC pause times
    group.bench_function("0.6_minor_gc", |b| b.iter(old_implementation));
    group.bench_function("0.7_minor_gc", |b| b.iter(new_implementation));
}
```

---

## 7. Risk Mitigation

### 7.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Remembered set memory overhead >5% | Medium | Medium | Benchmark with realistic workloads, allow configurable card size |
| Write barrier performance regression | Medium | High | Profile hot paths, optimize fast path, use atomic operations carefully |
| Race conditions in concurrent access | High | High | Extensive loom tests, thread sanitizer in CI, lock ordering discipline |
| Integration bugs with existing code | High | Medium | Incremental PRs, feature flag for testing, comprehensive integration tests |

### 7.2 Mitigation Strategies

#### Memory Overhead
- Start with conservative card size (512 bytes)
- Add configuration for tuning card size
- Benchmark on realistic game/UI workloads

#### Performance Regression
- Profile before/after on allocation-heavy benchmarks
- Add fast path for same-generation writes (no remembered set update)
- Consider incremental card marking

#### Race Conditions
- All concurrent access patterns tested with loom
- Lock ordering discipline documented and enforced
- Thread sanitizer (TSan) in CI pipeline

#### Integration Issues
- Incremental PRs after each phase
- Comprehensive integration tests before merge
- Documentation of migration path

### 7.3 Rollback Plan

If issues are found after release:
- All old APIs remain functional (no deprecation needed, per policy)
- Users can revert to previous behavior by avoiding write barriers
- No feature flag needed (generational GC is always-on)

---

## 8. Success Criteria

### 8.1 Functional Requirements

- [ ] Minor GC only scans remembered set, not entire heap
- [ ] All existing tests pass (./test.sh)
- [ ] New generational tests pass (100% coverage)
- [ ] No memory leaks or UAF in generational code paths (Miri clean)
- [ ] Handle edge cases: empty remembered set, full remembered set, concurrent access

### 8.2 Performance Requirements

| Metric | Target | Measurement |
|--------|--------|-------------|
| Minor GC pause (10MB young gen) | < 1ms | Benchmarks |
| Minor GC improvement vs 0.6 | 70-90% reduction | Comparison benchmark |
| Memory overhead | < 5% | Heap size with/without generational GC |
| Allocation throughput | No regression | Allocation benchmark |

### 8.3 Code Quality

- [ ] No clippy warnings (./clippy.sh passes)
- [ ] Documentation for new APIs (docs.rs)
- [ ] Examples in docs for generational GC usage
- [ ] README updated with generational GC documentation
- [ ] Code formatted (cargo fmt)

---

## 9. Timeline

```
Week 1:
  Mon-Wed: Phase 1 (Agent 1) - Data Structures
  Thu-Fri: Phase 2 starts (Agent 2) - Write Barrier

Week 2:
  Mon-Tue: Phase 2 completes (Agent 2)
  Mon-Wed: Phase 3 (Agent 3) - Minor Collection
  Thu-Fri: Phase 4 starts - Integration & Testing

Week 3:
  Mon-Wed: All - Integration, bug fixes, optimization
  Thu-Fri: Benchmarks, documentation, release prep
```

**Total Estimated Time**: 3 weeks with 3 agents

---

## 10. Agent Task Division

### Agent 1: Data Structures & Infrastructure

**Focus**: Foundation layer - card table, remembered set, configuration

**Deliverables**:
1. `src/gc/remembered_set.rs` - Core remembered set
2. `src/gc/card_table.rs` - Card table utilities
3. `src/gc/config.rs` - Configuration
4. Modifications to `src/heap.rs` (PageHeader, LocalHeap)
5. Unit tests for data structures

**Dependencies**: None (starts first)

**Key Files Modified**:
```
src/heap.rs
src/gc/remembered_set.rs (new)
src/gc/card_table.rs (new)
src/gc/config.rs (new)
tests/generation_card_table.rs (new)
```

---

### Agent 2: Write Barrier Integration

**Focus**: Mutation tracking - record old→young references

**Deliverables**:
1. Enhanced `GcCell::write_barrier`
2. Write barrier in `Gc<T>` clone/assign operations
3. Write barrier in derived Trace implementations
4. Integration tests for write barrier correctness
5. Documentation of barrier semantics

**Dependencies**: Waits for Agent 1's RememberedSet API (2 days)

**Key Files Modified**:
```
src/cell.rs
src/ptr.rs
src/gc/gc.rs (clone operations)
rudo-gc-derive/ (Trace impl updates)
tests/generation_write_barrier.rs (new)
```

---

### Agent 3: Minor Collection & Parallel GC

**Focus**: Collection algorithms - integrate RemSet with minor collection

**Deliverables**:
1. `mark_minor_roots_multi` with RemSet
2. `collect_minor_multi` parallel distribution
3. Integration with existing parallel marking
4. Metrics and configuration integration
5. Full integration tests

**Dependencies**: Waits for Agent 1 (3 days) and Agent 2 (2 days)

**Key Files Modified**:
```
src/gc/gc.rs
src/gc/marker.rs
src/metrics.rs
src/lib.rs
tests/generation_minor_collection.rs (new)
tests/generation_parallel.rs (new)
```

---

## 11. Open Questions (Resolved)

| Question | Resolution |
|----------|------------|
| Card size? | 512 bytes (Chez Scheme default) |
| Feature flag? | No, default-on |
| Backward compatibility? | Keep old APIs functional |
| Multi-generation? | No, stick to 2-generation for 0.7 |

---

## 12. References

- **Chez Scheme Implementation**: Reference for card table and remembered set design
- **Previous Implementations**: rudo-gc v0.6 partial generational GC
- **V8 GC**: Inspiration for 2-generation approach
- **AGENTS.md**: Project conventions and workflows

---

# Appendix A: 0.8 Draft - Incremental Marking

**Status**: Draft - Not Approved

This section outlines the planned features for version 0.8, following the completion of generational GC in 0.7.

## A.1 Overview

Version 0.8 will focus on **incremental marking** to further reduce pause times. While generational GC (0.7) targets short-lived objects, incremental marking targets the mark phase of major collections, which can still cause significant pauses for large object graphs.

## A.2 Goals

1. **Reduce major GC pause times**: Split mark phase into increments
2. **Cooperative scheduling**: Yield during long marking operations
3. **Concurrent marking**: Allow mutator work during marking
4. **Integration with generational**: Work with existing minor/major GC

## A.3 Architecture

### A.3.1 Write Barrier for Incremental Marking

```rust
/// Incremental write barrier - records mutations during mark phase
fn incremental_write_barrier(&self, new_value: &T) {
    // Check if GC is in progress
    if GC_MARK_IN_PROGRESS.load(Ordering::Acquire) {
        // Record the mutation for later processing
        let dirty_record = DirtyRecord {
            location: self.ptr.as_ptr(),
            value: new_value.ptr.as_ptr(),
        };
        INCREMENTAL_DIRTY_RECORDS.push(dirty_record);
    }

    // Also apply generational write barrier
    self.generational_write_barrier(new_value);
}
```

### A.3.2 Incremental Mark Phases

```
Phase 1: Snapshot (STW, short)
├── Capture all root references
├── Take snapshot of object graph
└── Set GC_MARK_IN_PROGRESS

Phase 2: Incremental Mark (cooperative)
├── Process worklist in chunks
├── Yield every N objects
├── Process dirty records from write barrier
└── Check if marking complete

Phase 3: Final Mark (STW, short)
├── Process remaining dirty records
├── Handle abandoned increments
└── Verify marking completeness

Phase 4: Sweep (can be lazy)
└── Standard sweep phase
```

### A.3.3 Dirty Record Buffer

```rust
struct DirtyRecord {
    location: *mut GcBox<()>,  // Where the pointer was written
    value: *const GcBox<()>,   // What was written
}

struct IncrementalDirtyRecords {
    records: Arc<Vec<DirtyRecord>>,
    write_index: AtomicUsize,
    read_index: AtomicUsize,
    chunk_size: usize,
}

impl IncrementalDirtyRecords {
    pub fn push(&self, record: DirtyRecord) {
        let idx = self.write_index.fetch_add(1, Ordering::Relaxed);
        self.records[idx % self.records.len()] = record;
    }

    pub fn pop_chunk(&self) -> &[DirtyRecord] {
        let write = self.write_index.load(Ordering::Acquire);
        let read = self.read_index.load(Ordering::Relaxed);

        if write - read >= self.chunk_size {
            let start = read;
            let end = read + self.chunk_size;
            self.read_index.store(end, Ordering::Release);
            &self.records[start % self.records.len()..end % self.records.len()]
        } else {
            &[]
        }
    }
}
```

## A.4 Implementation Phases (0.8)

### Phase A1: Write Barrier Foundation
- Implement incremental write barrier
- Add dirty record buffer
- Test barrier correctness

### Phase A2: Snapshot Mechanism
- Implement snapshot-at-beginning (SATB)
- Add GC state tracking
- Integrate with existing mark infrastructure

### Phase A3: Incremental Mark Loop
- Modify worker mark loop to process increments
- Add yield points
- Handle work stealing with incremental state

### Phase A4: Integration
- Integrate with generational GC
- Add configuration options
- Comprehensive testing

## A.5 Timeline (0.8)

```
Week 1:
  Mon-Wed: Phase A1 - Write Barrier
  Thu-Fri: Phase A2 - Snapshot

Week 2:
  Mon-Wed: Phase A3 - Incremental Loop
  Thu-Fri: Phase A4 - Integration

Week 3:
  Mon-Wed: Testing, bug fixes
  Thu-Fri: Benchmarks, docs, release prep
```

**Total Estimated Time**: 3 weeks with 3 agents (similar to 0.7)

## A.6 Success Criteria (0.8)

| Metric | Target |
|--------|--------|
| Major GC pause reduction | 50-80% |
| Mutator utilization | >90% during marking |
| Memory overhead | <3% for dirty records |
| No correctness bugs | Miri clean |

---

## Appendix B: Chez Scheme Reference Notes

### B.1 Card Table Design

Chez Scheme uses a card-based remembered set with the following characteristics:

- **Card Size**: 512 bytes (multiple of pointer size)
- **Dirty Byte**: One byte per card stores youngest generation pointed to
- **Dirty Segment Lists**: Per-generation-pair linked lists for efficient scanning
- **New Dirty Cards Buffer**: Captures writes during collection

### B.2 Key Differences from rudo-gc 0.7

| Aspect | Chez Scheme | rudo-gc 0.7 |
|--------|-------------|-------------|
| Generations | 6 (configurable) | 2 (young/old) |
| Promotion | Incremental (one gen at a time) | Direct (young→old) |
| Card Size | 512 bytes | 512 bytes |
| Remembered Set | Per-generation-pair | Per-thread |
| Parallelism | Segment ownership | Work stealing |

### B.3 Implementation Patterns to Adopt

1. **Card Index Calculation**: `card_index = (ptr - segment_base) / CARD_SIZE`
2. **Dirty Byte Encoding**: 0=clean, 1-255=youngest generation pointed to
3. **Segment Lists**: Maintain separate lists for each generation pair
4. **Parallel Sweeping**: Each segment owned by allocating thread

---

*Document generated: 2026-02-02*
*Plan approved: TBD*
*Implementation start: TBD*
