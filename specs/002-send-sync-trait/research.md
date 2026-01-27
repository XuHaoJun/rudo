# Research: Send + Sync Trait Implementation

**Date**: 2026-01-27  
**Feature**: Send + Sync Trait Support (`002-send-sync-trait`)

---

## Decision: Atomic Reference Counting for Thread Safety

### Summary

To enable `Send` and `Sync` for `Gc<T>` and `Weak<T>`, we must replace non-atomic `Cell` types with atomic alternatives. This enables safe concurrent access while maintaining the garbage collector's correctness guarantees.

### Memory Ordering Strategy

Based on analysis of ChezScheme's atomic operations (`learn-projects/ChezScheme/c/atomic.h`):

| Operation | Ordering | Cost | Rationale |
|-----------|----------|------|-----------|
| `inc_ref` | `Relaxed` | Free (x86) / Low | Counter only, no synchronization needed |
| `dec_ref` | `AcqRel` | Medium | Must synchronize: release our ref, acquire others' drops |
| `inc_weak` | `Relaxed` | Free (x86) / Low | Advisory only, no correctness impact |
| `dec_weak` | `AcqRel` | Medium | Must synchronize weak count for upgrade() correctness |
| Pointer load | `Acquire` | Free (x86) / Medium | Must see fully initialized object |
| Pointer store | `Release` | Free (x86) / Medium | Must make initialization visible to other threads |

### ChezScheme Reference

ChezScheme uses platform-specific memory barriers (`c/atomic.h`):

```c
// x86_64 - Strong memory model
#define ACQUIRE_FENCE() do { } while (0)  // No barrier needed
#define RELEASE_FENCE() do { } while (0)

// ARM64 - Weak memory model  
#define ACQUIRE_FENCE() __asm__ __volatile__ ("dmb ish" : : : "memory")
#define RELEASE_FENCE() __asm__ __volatile__ ("dmb ish" : : : "memory")
```

Rust's `std::sync::atomic` handles these differences automatically.

---

## Alternative Approaches Considered

### 1. External Atomic Cell Library

**Option**: Use `crossbeam::AtomicCell` or similar

**Verdict**: Rejected
- Adds external dependency
- `AtomicUsize` in stdlib is sufficient
- Extra dependency increases maintenance burden

### 2. Mutex-Protected Reference Counting

**Option**: Wrap `Cell` in `Mutex<T>`

**Verdict**: Rejected
- Unnecessary synchronization overhead
- Lock-free atomic operations are faster
- Violates performance-first design principle

### 3. Custom CAS Loop

**Option**: Implement custom Compare-And-Swap logic

**Verdict**: Rejected
- Error-prone implementation
- Less maintainable than std atomic types
- Standard library is well-tested and optimized

---

## Reference Implementation Analysis

### dumpster (learn-projects/dumpster/dumpster/src/sync/mod.rs)

```rust
pub struct Gc<T: Trace + Send + Sync + ?Sized + 'static> {
    ptr: UCell<Nullable<GcBox<T>>>,
    tag: AtomicUsize,
}

pub struct GcBox<T>
where T: Trace + Send + Sync + ?Sized {
    strong: AtomicUsize,
    weak: AtomicUsize,
    generation: AtomicUsize,
    value: T,
}

unsafe impl<T> Send for Gc<T> where T: Trace + Send + Sync + ?Sized {}
unsafe impl<T> Sync for Gc<T> where T: Trace + Send + Sync + ?Sized {}
```

**Key Insights**:
- Uses `UCell` (atomic cell) for pointer storage
- Separate `tag` field for mutation detection
- Conditional trait bounds: `T: Trace + Send + Sync`

---

## Implementation Details

### Reference Count Overflow Handling

**Decision**: Saturating counter at `isize::MAX`

Rationale:
- Prevents undefined behavior from overflow
- Predictable semantics
- Matches common patterns in production systems

### CAS Loop Strategy

**Decision**: Exponential backoff on contention

```rust
fn dec_ref(&self) -> bool {
    loop {
        let current = self.ref_count.load(Ordering::Relaxed);
        if current == 1 {
            // Last reference - drop value
            // ... drop logic
            return true;
        }
        // Attempt to decrement
        match self.ref_count.compare_exchange_weak(
            current,
            current - 1,
            Ordering::AcqRel,
            Ordering::Relaxed
        ) {
            Ok(_) => return false,
            Err(_) => {
                // Exponential backoff
                backoff.spin();
            }
        }
    }
}
```

---

## Compatibility Analysis

### Internal vs External Breaking Changes

| Component | Type | Impact |
|-----------|------|--------|
| `GcBox::ref_count` | Internal | Hidden from public API |
| `GcBox::weak_count` | Internal | Hidden from public API |
| `Gc::ptr` | Internal | Hidden from public API |
| Trait bounds | External | Additive, not breaking |

### Backward Compatibility

- Existing single-threaded code works unchanged
- New multi-threaded capability is additive
- Internal representation changes are encapsulated

---

## Testing Strategy

### Unit Tests

- Atomic ref count operations
- Memory ordering correctness
- Reference count overflow handling

### Integration Tests

- Multi-threaded clone/drop stress test
- Weak reference upgrade across threads
- GC collection during concurrent access

### Verification

- `assert_send_and_sync::<Gc<Arc<AtomicUsize>>>()` compile-time check
- ThreadSanitizer for data race detection
- Miri for memory safety verification

---

## Conclusion

The implementation approach is well-supported by:

1. **Theory**: Atomic operations are standard for thread-safe reference counting
2. **Practice**: `dumpster` and other GC implementations use this pattern
3. **Platform support**: Rust's `std::sync::atomic` handles cross-platform differences
4. **Conformance**: Satisfies all rudo-gc constitution requirements

Proceed to implementation phase.
