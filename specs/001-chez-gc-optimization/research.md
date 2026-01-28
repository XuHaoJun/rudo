# Research Findings: Chez Scheme GC optimizations for rudo-gc

## Overview

This document consolidates research findings from analyzing the Chez Scheme garbage collector implementation at `/learn-projects/ChezScheme/c/` to inform the implementation of five optimizations for rudo-gc.

## Decision: Mark Bitmap Implementation

**Decision**: Replace forwarding pointers with page-level mark bitmap.

**Rationale**:
- Chez Scheme uses `marked_mask` (octet pointer) in `seginfo` struct (types.h:168) with one bit per pointer-sized unit
- Forwarding pointers add 8 bytes overhead per object (on 64-bit systems); bitmap adds 1 bit per pointer slot
- For a 4KB page with 512 pointer slots, bitmap uses 64 bytes vs 4096 bytes for per-object forwarding
- Mark bitmap enables true parallel marking without coordinating per-object updates

**Implementation approach** (based on gc.c:73-105):
- Add `PageBitmap` struct with `Vec<u64>` for bitmap storage
- One bit per pointer-sized unit; word-level operations for efficiency
- `marked_count` field tracks marked bytes for sweep phase optimization
- Mark phase sets bits; sweep phase reads bitmap for liveness

**Alternatives considered**:
- Keep forwarding pointers (rejected: per-object overhead too high)
- Hybrid approach with both (rejected: complexity without benefit)

---

## Decision: Push-Based Work Transfer

**Decision**: Implement notification-based work transfer using condition variables.

**Rationale**:
- Chez Scheme `push_remote_sweep` function (gc.c:1815-1826) pushes remote references to owner's stack
- Uses stack-based protocol with `send_remote_sweep_stack` and `receive_remote_sweep_stack`
- Condition variables wake sleeping workers (SWEEPER_WAITING_FOR_WORK state)
- Reduces contention vs polling-based stealing

**Implementation approach** (based on gc.c:1815-1871):
- Add `pending_work: Mutex<Vec<MarkWork>>` to `PerThreadMarkQueue`
- Add `work_available: Notify` for worker notification
- `push_remote()`: acquires lock, pushes work, notifies worker
- `receive_work()`: drains pending work when local queue empty

**Buffer sizing** (per spec clarification):
- Fixed buffer of 8-16 items to minimize memory overhead
- Prioritizes low memory footprint over notification frequency

---

## Decision: Segment Ownership for Load Distribution

**Decision**: Integrate page ownership into work distribution to prioritize local work.

**Rationale**:
- Chez Scheme `seginfo.creator` field (types.h:155) tracks owning thread for each segment
- During parallel GC, sweeper must own segment to mark/copy (gc.c:138-157)
- Non-owning sweeper encountering remote reference pushes to owner's queue
- Prevents false sharing and improves cache locality

**Implementation approach** (based on types.h:154-156):
- `PageHeader.owner_thread` field already exists; complete integration
- Add `owned_pages: HashSet<PagePtr>` to `PerThreadMarkQueue`
- When marking owned page, push directly to local queue
- When encountering remote page, push to owner's `pending_work`
- Stealing prioritizes queues of page owners

**Page size constraint** (per spec clarification):
- Uniform page sizes required for ownership distribution
- Simplifies implementation; clear contract with users

---

## Decision: Lock Ordering Discipline

**Decision**: Define and enforce systematic lock acquisition order.

**Rationale**:
- Chez Scheme uses `GC_MUTEX_ACQUIRE()` mapped to `alloc_mutex_acquire()` (gc.c:393)
- Thread context mutex acquired before allocation mutex
- Prevents circular wait condition that causes deadlock

**Implementation approach** (based on gc.c:393-394):
- Document lock ordering: LocalHeap -> GlobalMarkState -> GC Request
- Never acquire LocalHeap while holding GlobalMarkState
- Never acquire GlobalMarkState while holding GC Request
- Add lock order assertions in debug builds using `AtomicU8` tags

---

## Decision: Dynamic Stack Growth

**Decision**: Implement pre-allocated stack with growth notification.

**Rationale**:
- Chez Scheme `push_sweep` macro (gc.c:337-342) checks stack limit before push
- `enlarge_stack` function handles growth dynamically
- Pre-allocation prevents stalls under load

**Implementation approach** (based on gc.c:335-342):
- Add `capacity_hint: AtomicUsize` to `PerThreadMarkQueue`
- Monitor queue capacity utilization
- Pre-allocate additional slots when threshold exceeded
- Push to remote `pending_work` as overflow strategy

---

## Implementation Order (from optimization plan)

### Phase 1: Immediate Improvements (1-2 days)

1. **Lock Ordering Documentation** - Prevents future bugs
2. **Push-Based Work Transfer** - Reduces steal contention
3. **Segment Ownership Integration** - Better load distribution

### Phase 2: Structural Changes (1 week)

4. **Mark Bitmap** - Enables true parallel marking
5. **Dynamic Stack Growth** - Better throughput under load

---

## Key Code References

| Pattern | Chez Scheme File | Lines |
|---------|------------------|-------|
| Mark bitmap | `gc.c` | 73-105 |
| Segment ownership | `types.h` | 154-156 |
| Push-based transfer | `gc.c` | 1815-1871 |
| Lock ordering | `gc.c` | 393-394 |
| Dynamic stack growth | `gc.c` | 335-342 |
| Seginfo structure | `types.h` | 143-185 |

---

## Testing Strategy

Based on research and spec requirements:

1. **Unit tests**: PerThreadMarkQueue operations, MarkBitmap operations
2. **Integration tests**: Multi-threaded marking with contention, lock ordering validation
3. **Miri tests**: All unsafe code involving raw pointers
4. **Benchmarks**: GC pause time reduction, memory overhead reduction
5. **Stress tests**: 24-hour concurrent GC cycles with randomized workloads

---

## Open Questions (All Resolved)

| Question | Resolution | Source |
|----------|------------|--------|
| Mark bitmap vs forwarding pointers | Complete replacement | User clarification Q1 |
| Work queue buffer size | Fixed 8-16 items | User clarification Q2 |
| Heterogeneous page sizes | Uniform pages required | User clarification Q3 |
