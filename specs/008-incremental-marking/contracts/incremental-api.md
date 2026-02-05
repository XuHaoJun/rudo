# API Contract: Incremental Marking

**Feature**: 008-incremental-marking  
**Date**: 2026-02-03  
**Status**: Complete

This document defines the internal API contracts for incremental marking.

---

## 1. Public API

### 1.1 Configuration

```rust
/// Configuration for incremental marking behavior.
/// 
/// # Example
/// 
/// ```rust
/// use rudo_gc::IncrementalConfig;
/// 
/// let config = IncrementalConfig {
///     enabled: true,
///     increment_size: 2000,  // Mark 2000 objects per slice
///     ..Default::default()
/// };
/// rudo_gc::set_incremental_config(config);
/// ```
pub struct IncrementalConfig {
    /// Enable incremental marking for major collections.
    /// Default: `false` (opt-in)
    pub enabled: bool,
    
    /// Objects to mark per increment.
    /// Lower values = shorter pauses but more overhead.
    /// Default: 1000
    pub increment_size: usize,
    
    /// Maximum dirty pages before falling back to STW.
    /// Default: 1000
    pub max_dirty_pages: usize,
    
    /// Per-thread remembered buffer size.
    /// Default: 32
    pub remembered_buffer_len: usize,
    
    /// Slice timeout in milliseconds.
    /// Default: 50
    pub slice_timeout_ms: u64,
}

impl Default for IncrementalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            increment_size: 1000,
            max_dirty_pages: 1000,
            remembered_buffer_len: 32,
            slice_timeout_ms: 50,
        }
    }
}

/// Set the incremental marking configuration.
/// 
/// Must be called before any GC activity.
/// 
/// # Panics
/// 
/// Panics if called after GC has been initialized.
pub fn set_incremental_config(config: IncrementalConfig);

/// Get the current incremental marking configuration.
pub fn get_incremental_config() -> &'static IncrementalConfig;
```

### 1.2 Cooperative Scheduling

```rust
impl<T: Trace + ?Sized> Gc<T> {
    /// Yield to the garbage collector if incremental marking is in progress.
    /// 
    /// Call this periodically in long-running computations to allow
    /// incremental marking to make progress.
    /// 
    /// # Example
    /// 
    /// ```rust
    /// fn process_large_dataset(data: &[Item]) {
    ///     for (i, item) in data.iter().enumerate() {
    ///         process_item(item);
    ///         if i % 1000 == 0 {
    ///             Gc::<()>::yield_now();  // Allow GC to mark
    ///         }
    ///     }
    /// }
    /// ```
    /// 
    /// # Performance
    /// 
    /// This is a no-op if incremental marking is not active.
    /// When active, it executes one mark slice.
    pub fn yield_now();
}
```

### 1.3 GC Trigger

```rust
/// Trigger a major garbage collection.
/// 
/// If incremental marking is enabled, this starts incremental marking.
/// Otherwise, performs a stop-the-world major collection.
/// 
/// # Example
/// 
/// ```rust
/// // Force a major collection
/// rudo_gc::collect_major();
/// ```
pub fn collect_major();

/// Check if incremental marking is currently in progress.
/// 
/// # Returns
/// 
/// `true` if the GC is in the `Marking` phase.
pub fn is_incremental_marking_active() -> bool;
```

---

## 2. Internal API

### 2.1 IncrementalMarkState

```rust
impl IncrementalMarkState {
    /// Get or initialize the global incremental mark state.
    pub fn global() -> &'static Self;
    
    /// Get current marking phase.
    /// 
    /// # Thread Safety
    /// 
    /// Uses atomic load, safe to call from any thread.
    pub fn phase(&self) -> MarkPhase;
    
    /// Transition to a new phase.
    /// 
    /// # Panics
    /// 
    /// Panics if the transition is invalid according to the state machine.
    pub fn transition_to(&self, new_phase: MarkPhase);
    
    /// Push an object onto the global worklist.
    /// 
    /// # Thread Safety
    /// 
    /// Lock-free, safe to call from any thread.
    pub fn push_work(&self, ptr: NonNull<GcBox<()>>);
    
    /// Pop an object from the global worklist.
    /// 
    /// # Returns
    /// 
    /// `None` if the worklist is empty.
    pub fn pop_work(&self) -> Option<NonNull<GcBox<()>>>;
    
    /// Check if the worklist is empty.
    pub fn worklist_is_empty(&self) -> bool;
    
    /// Request fallback to STW completion.
    /// 
    /// Called when thresholds are exceeded.
    pub fn request_fallback(&self, reason: FallbackReason);
    
    /// Check if fallback was requested.
    pub fn fallback_requested(&self) -> bool;
    
    /// Reset state for new collection cycle.
    /// 
    /// # Precondition
    /// 
    /// Must be in `Idle` or `Sweeping` phase.
    pub fn reset(&self);
}
```

### 2.2 Mark Slice Execution

```rust
/// Execute one incremental mark slice.
/// 
/// # Parameters
/// 
/// - `heap`: Thread's local heap
/// - `budget`: Maximum objects to mark this slice
/// 
/// # Returns
/// 
/// Result indicating slice outcome.
/// 
/// # Contract
/// 
/// - Caller must be a mutator thread (not GC worker during STW)
/// - Called when `phase == Marking`
/// - Marks up to `budget` objects from worklist
/// - Processes dirty pages if worklist empty
pub fn mark_slice(
    heap: &LocalHeap,
    budget: usize,
) -> MarkSliceResult;

/// Execute the snapshot phase.
/// 
/// # Contract
/// 
/// - Must be called with all mutators stopped (STW)
/// - Captures all roots into worklist
/// - Clears mark bits
/// - Transitions to `Marking` phase
/// 
/// # Thread Safety
/// 
/// Must be called from GC coordinator thread only.
pub fn execute_snapshot(
    heaps: &[&LocalHeap],
) -> usize; // Returns number of roots captured

/// Execute the final mark phase.
/// 
/// # Contract
/// 
/// - Must be called with all mutators stopped (STW)
/// - Processes remaining dirty pages
/// - Completes any remaining marking work
/// - Transitions to `Sweeping` phase
/// 
/// # Returns
/// 
/// Total objects marked during final phase.
pub fn execute_final_mark(
    heaps: &[&LocalHeap],
) -> usize;
```

### 2.3 Write Barrier

```rust
/// Combined write barrier for generational + incremental GC.
/// 
/// # Parameters
/// 
/// - `source`: Object containing the slot being written
/// - `old_value`: Previous pointer value (for SATB)
/// - `new_value`: New pointer value being written
/// 
/// # Contract
/// 
/// - Must be called BEFORE the actual write
/// - `old_value` may be null if slot was uninitialized
/// - Handles both old→young (generational) and SATB (incremental)
/// 
/// # Performance
/// 
/// Fast path: single atomic load to check if barriers needed.
/// Slow path: per-thread buffer with batch flush.
#[inline]
pub fn write_barrier<T: Trace>(
    source: *const GcBox<()>,
    old_value: Option<NonNull<GcBox<()>>>,
    new_value: Option<NonNull<GcBox<()>>>,
);

/// Check if any write barrier is active.
/// 
/// # Returns
/// 
/// `true` if incremental marking is active OR
/// `true` if generational GC is enabled.
#[inline]
pub fn write_barrier_needed() -> bool;
```

### 2.5 GcCell SATB Barrier API

```rust
impl<T> GcCell<T> {
    /// Mutably borrows the wrapped value with generational, incremental, and SATB barriers.
    ///
    /// **Barrier Type**: Full (Generational + Incremental + SATB)
    ///
    /// This method captures old GC pointer values before mutation, enabling correct
    /// incremental marking. Uses `GcCapture` trait to capture old pointer values.
    ///
    /// **Use Case**: General purpose - works with types that implement GcCapture
    ///
    /// # Type Bounds
    ///
    /// - `T: GcCapture` - Required for SATB barrier. Use `borrow_mut_gen_only()` for
    ///   types without GC pointers to avoid the SATB overhead.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Works with types containing GC pointers:
    /// let cell = GcCell::new(Some(Gc::new(Data)));
    /// *cell.borrow_mut() = Some(Gc::new(new_data));  // SATB active
    ///
    /// // For types without GC pointers:
    /// let cell = GcCell::new(42);
    /// *cell.borrow_mut() = 100;  // Only generational barrier needed
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    pub fn borrow_mut(&self) -> RefMut<'_, T>
    where
        T: GcCapture;

    /// Mutably borrows the wrapped value with generational barrier only.
    ///
    /// **Barrier Type**: Generational only
    ///
    /// This is an escape hatch for performance-critical code where SATB
    /// barrier overhead is measurable. No SATB recording is performed.
    ///
    /// **Warning**: Using this on types containing GC pointers during incremental
    /// marking may cause reachable objects to be incorrectly collected.
    ///
    /// **Use Case**: Performance optimization - for types without GC pointers
    ///
    /// # Example
    ///
    /// ```ignore
    /// let cell = GcCell::new(expensive_computation());
    /// *cell.borrow_mut_gen_only() = result;  // Only generational barrier
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T>;
}
```

### 2.6 Thread-Local Operations

```rust
impl ThreadControlBlock {
    /// Push work to thread-local mark queue.
    /// 
    /// Overflow to global worklist when local queue is full.
    pub fn push_local_mark_work(&mut self, ptr: NonNull<GcBox<()>>);
    
    /// Pop work from thread-local mark queue.
    /// 
    /// Tries to steal from global worklist if local is empty.
    pub fn pop_local_mark_work(&mut self) -> Option<NonNull<GcBox<()>>>;
    
    /// Record a page in the remembered buffer.
    /// 
    /// Flushes to global dirty list on overflow.
    pub fn record_in_remembered_buffer(&mut self, page: NonNull<PageHeader>);
    
    /// Flush remembered buffer to global dirty list.
    pub fn flush_remembered_buffer(&mut self);
    
    /// Get count of objects marked this slice.
    pub fn marked_this_slice(&self) -> usize;
    
    /// Reset slice-local counters.
    pub fn reset_slice_counters(&mut self);
    
    /// Enable/disable work-stealing for current slice.
    /// 
    /// Called at slice boundaries to prevent stealing across slices.
    pub fn set_stealing_allowed(&mut self, allowed: bool);
    
    /// Check if work-stealing is currently allowed.
    pub fn stealing_allowed(&self) -> bool;
}
```

---

## 3. Error Handling

### 3.1 Fallback Conditions

| Condition | Detection | Action |
|-----------|-----------|--------|
| Dirty pages exceed `max_dirty_pages` | `dirty_pages.len() > config.max_dirty_pages` | `request_fallback(DirtyPagesExceeded)` |
| Slice timeout | `slice_start.elapsed() > config.slice_timeout_ms` | `request_fallback(SliceTimeout)` |
| Worklist unbounded growth | `worklist.len() > 10 * initial_worklist_len` | `request_fallback(WorklistUnbounded)` |

### 3.2 Recovery

When fallback is triggered:

1. Coordinator detects `fallback_requested()`
2. Stop all mutators
3. Complete remaining marking STW
4. Proceed to sweep
5. Log fallback reason for diagnostics

---

## 4. Thread Safety Contracts

| Component | Synchronization | Access Pattern |
|-----------|-----------------|----------------|
| `IncrementalMarkState.phase` | `AtomicUsize` | Read: any thread. Write: coordinator only |
| `IncrementalMarkState.worklist` | `crossbeam::SegQueue` | Lock-free MPMC |
| `LocalHeap.dirty_pages` | `parking_lot::Mutex` | Mutex-protected |
| `ThreadControlBlock.local_mark_queue` | None (thread-local) | Owner thread only |
| `ThreadControlBlock.remembered_buffer` | None (thread-local) | Owner thread only |
| `PageHeader.mark_bits` | `AtomicU64` array | Atomic CAS |

---

## 5. Performance Contracts

| Operation | Expected Cost | Bound |
|-----------|---------------|-------|
| `is_incremental_marking_active()` | 1 atomic load | O(1) |
| `write_barrier()` fast path | 2 atomic loads | O(1) |
| `write_barrier()` slow path | Buffer insert + possible flush | O(1) amortized |
| `mark_slice()` | Object marking | O(budget) |
| `execute_snapshot()` | Root capture | O(roots) |
| `execute_final_mark()` | Dirty page scan | O(dirty_pages) |

---

*Generated by /speckit.plan | 2026-02-03*
