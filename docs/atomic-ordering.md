# Atomic Ordering Policy for rudo-gc

## Document Information

- **Created**: 2026-01-30
- **Author**: Based on code review by Rust leadership council
- **Version**: 1.0
- **Related**: ATOMIC-ORDER-2024 fix plan

## Executive Summary

This document describes the memory ordering (atomic ordering) guarantees required for correct operation of rudo-gc's concurrent data structures. All atomic operations must follow these rules to ensure memory safety and correctness under concurrent access.

## Risk Classification

| Level | Description | Response |
|-------|-------------|----------|
| **P0** | Memory safety - potential use-after-free or data race | Critical fix required |
| **P1** | Correctness - logical errors in concurrent state | High priority fix |
| **P2** | Monitoring accuracy - metrics/statistics inconsistency | Medium priority fix |

## Ordering Requirements by Module

### 1. Work-Stealing Queue (gc/worklist.rs)

The Chase-Lev work-stealing algorithm requires specific memory ordering to maintain correctness.

#### Push Operation

```rust
pub fn push(&self, bottom: &AtomicUsize, item: T) -> bool {
    // ... load checks ...
    unsafe {
        (*self.buffer.get())[index].write(item);
    }
    bottom.store(b.wrapping_add(1), Ordering::Release);
    true
}
```

**Requirement**: `bottom` store must use `Release` ordering.

**Rationale**: The data write must be visible to stealing threads before `bottom` is incremented. Without `Release` ordering:
- A stealing thread might see `bottom` incremented but not the data
- This could lead to reading uninitialized memory or incorrect values

#### Pop Operation

```rust
pub fn pop(&self, bottom: &AtomicUsize) -> Option<T> {
    // ... load checks ...
    bottom.store(new_b, Ordering::Release);
    // ... read data ...
    if new_b != t {
        return Some(item);
    }
    // Last item special case
    bottom.store(b, Ordering::Release);  // or bottom.store(t.wrapping_add(1), Release)
    Some(item)
}
```

**Requirement**: All `bottom` stores in `pop()` must use `Release` ordering.

#### Steal Operation

```rust
pub fn steal(&self, bottom: &AtomicUsize) -> Option<T> {
    let t = self.top.load(Ordering::Acquire);
    let b = bottom.load(Ordering::Relaxed);
    // ...
}
```

**Requirement**: `top` load must use `Acquire` ordering; `bottom` load can use `Relaxed`.

**Rationale**: `Acquire` on `top` ensures we see any pushes that completed before the CAS. `Relaxed` on `bottom` is acceptable since it's only used for empty/full checks.

---

### 2. Reference Counting (ptr.rs)

Reference counting requires careful ordering to prevent use-after-free.

#### ref_count()

```rust
pub fn ref_count(&self) -> NonZeroUsize {
    NonZeroUsize::new(self.ref_count.load(Ordering::Acquire))
        .expect("ref_count should never be zero for live GcBox")
}
```

**Requirement**: Must use `Acquire` ordering.

**Rationale**: Ensures we see the complete effect of any prior decrements. Without `Acquire`, we might observe a stale count value that hasn't been synchronized, potentially causing premature collection.

#### inc_ref()

```rust
pub fn inc_ref(&self) {
    self.ref_count
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            if count == usize::MAX { None } else { Some(count.saturating_add(1)) }
        })
        .ok();
}
```

**Requirement**: Can use `Relaxed` ordering.

**Rationale**: Increment is a "strengthening" operation that only increases the count. The critical ordering is on the decrement path, which uses `Acquire` to synchronize.

#### dec_ref()

```rust
pub fn dec_ref(self_ptr: *mut Self) -> bool {
    let count = this.ref_count.load(Ordering::Acquire);
    // ... check and potentially drop ...
    self.ref_count
        .compare_exchange_weak(count, count - 1, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
}
```

**Requirement**: Load uses `Acquire`, CAS uses `AcqRel`.

**Rationale**: `Acquire` ensures we see any writes before the decrement. `AcqRel` on CAS ensures the decrement is visible to other threads while acquiring any state needed for the check.

---

### 3. Dropping State (ptr.rs)

The dropping state prevents races between weak reference upgrades and object destruction.

#### dropping_state()

```rust
fn dropping_state(&self) -> usize {
    self.is_dropping.load(Ordering::Acquire)
}
```

**Requirement**: Must use `Acquire` ordering.

**Rationale**: Ensures we see any prior calls to `try_mark_dropping()`. Without this, a thread might observe `state == 0` when another thread has already marked dropping, leading to a race.

#### try_mark_dropping()

```rust
fn try_mark_dropping(&self) -> bool {
    self.is_dropping
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}
```

**Requirement**: Both success and failure ordering must be `Acquire` (or `AcqRel` for success).

**Rationale**: Even a failed CAS provides synchronization - it ensures we see other threads' writes to the atomic. The failure ordering being `Acquire` is critical for correctness.

---

### 4. Under Construction Flag (ptr.rs)

The under-construction flag coordinates object construction completion.

#### set_under_construction()

```rust
fn set_under_construction(&self, flag: bool) {
    let mask = Self::UNDER_CONSTRUCTION_FLAG;
    if flag {
        self.weak_count.fetch_or(mask, Ordering::Release);
    } else {
        self.weak_count.fetch_and(!mask, Ordering::AcqRel);
    }
}
```

**Requirement**: Setting uses `Release`, clearing uses `AcqRel`.

**Rationale**: Setting the flag only needs `Release` - it signals "construction in progress". Clearing needs `AcqRel` to synchronize with readers who need to see the completion of construction.

---

### 5. GC Request (heap.rs)

Global GC request coordination.

#### check_safepoint()

```rust
pub fn check_safepoint() {
    if GC_REQUESTED.load(Ordering::Acquire) && !crate::gc::is_collecting() {
        enter_rendezvous();
    }
}
```

**Requirement**: Must use `Acquire` ordering.

**Rationale**: Ensures we see the complete GC request state, including any associated metadata or state changes that accompany the request.

---

### 6. Metrics (gc/marker.rs)

Statistics and metrics collection.

#### total_marked() / record_marked()

```rust
pub fn total_marked(&self) -> usize {
    self.total_marked.load(Ordering::Acquire)
}

pub fn record_marked(&self, count: usize) {
    self.total_marked.fetch_add(count, Ordering::Release);
}
```

**Requirement**: Load uses `Acquire`, store uses `Release`.

**Rationale**: `Acquire` ensures we see all prior marks. `Release` ensures our mark is visible to readers. While metrics aren't critical for memory safety, consistent ordering prevents confusing metric values.

---

## Fix Summary

### P0 (Memory Safety) - Completed

| File | Line | Before | After | Status |
|------|------|--------|-------|--------|
| gc/worklist.rs | 86 | `Relaxed` | `Release` | Fixed |
| gc/worklist.rs | 114 | `Relaxed` | `Release` | Fixed |
| gc/worklist.rs | 134 | `Relaxed` | `Release` | Fixed |
| gc/worklist.rs | 138 | `Relaxed` | `Release` | Fixed |
| ptr.rs | 49 | `Relaxed` | `Acquire` | Fixed |

### P1 (Correctness) - Completed

| File | Line | Before | After | Status |
|------|------|--------|-------|--------|
| ptr.rs | 74 | `Relaxed` | `Acquire` | Fixed |
| ptr.rs | 82 | `Relaxed` (failure) | `Acquire` | Fixed |
| ptr.rs | 64 | `Relaxed` | `Release` | Fixed |
| ptr.rs | 66 | `Relaxed` | `AcqRel` | Fixed |
| heap.rs | 187 | `Relaxed` | `Acquire` | Fixed |

### P2 (Monitoring) - Completed

| File | Line | Before | After | Status |
|------|------|--------|-------|--------|
| gc/marker.rs | 828 | `Relaxed` | `Release` | Fixed |

---

## Future Guidelines

When adding new atomic operations to rudo-gc:

1. **For "read-then-act" patterns**: Use `Acquire` on the read
2. **For "publish-then-notify" patterns**: Use `Release` on the publish
3. **For CAS operations**: Consider both success and failure orderings
   - Success: Usually `AcqRel` or `Release`
   - Failure: Often needs `Acquire` to synchronize state
4. **For pure counters**: `Relaxed` is usually acceptable
   - But if the counter guards state, stronger ordering may be needed

### Common Patterns

```rust
// Pattern 1: Check state before acting
let state = atomic.load(Acquire);
if state == SOME_VALUE {
    // Act on state - Acquire ensures we see all related writes
}

// Pattern 2: Update state and notify
atomic.store(new_value, Release);
// Release ensures new_value is visible before/with the notification

// Pattern 3: Conditional update
atomic.compare_exchange(old, new, AcqRel, Acquire);
// Both orderings matter - success needs visibility, failure needs sync
```

---

## Testing

All atomic ordering changes should be validated with:

1. **Miri**: Detects UB in concurrent code
   ```bash
   cargo +nightly miri test --lib
   ```

2. **Loom**: Systematic exploration of thread interleavings
   ```bash
   cargo test loom_ --release
   ```

3. **Stress tests**: Long-running concurrent workloads
   ```bash
   cargo test stress_test --release -- --test-threads=1
   ```

---

## References

- [Rust Atomics and Locks](https://marabos.nl/atomics/) by Mara Bos
- [C++ Memory Model](https://en.cppreference.com/w/cpp/atomic/memory_order)
- [Chase-Lev Work-Stealing Algorithm](https://dl.acm.org/doi/10.1145/1074013.1074017)
- [Linux Kernel Memory Barriers](https://www.kernel.org/doc/Documentation/memory-barriers.txt)
