# API Contracts: Parallel Marking

**Feature**: Parallel Marking  
**Date**: 2026-01-27

---

## Public API

### Module: `gc::worklist`

#### `StealQueue<T, const N: usize>`

```rust
pub struct StealQueue<T, const N: usize> {
    buffer: [std::mem::MaybeUninit<T>; N],
    bottom: std::cell::Cell<usize>,
    top: std::sync::atomic::AtomicUsize,
    mask: usize,
}

impl<T: Copy, const N: usize> StealQueue<T, N>
where
    N: num_traits::Pow<usize, Output = usize>,
{
    /// Create a new steal queue (N must be a power of 2)
    ///
    /// # Panics
    ///
    /// Panics if N is not a power of 2.
    pub const fn new() -> Self;

    /// Push an item to the local end (LIFO)
    ///
    /// Returns `true` if successful, `false` if queue is full.
    pub fn push(&self, bottom: &std::cell::Cell<usize>, item: T) -> bool;

    /// Pop an item from the local end (LIFO)
    ///
    /// Returns `Some(item)` if successful, `None` if queue is empty.
    pub fn pop(&self, bottom: &std::cell::Cell<usize>) -> Option<T>;

    /// Steal an item from the remote end (FIFO)
    ///
    /// Returns `Some(item)` if successful, `None` if queue is empty.
    pub fn steal(&self) -> Option<T>;

    /// Get the current size of the queue
    pub fn len(&self, bottom: &std::cell::Cell<usize>) -> usize;

    /// Check if the queue is empty
    pub fn is_empty(&self, bottom: &std::cell::Cell<usize>) -> bool;

    /// Check if the queue is full
    pub fn is_full(&self, bottom: &std::cell::Cell<usize>) -> bool;
}
```

---

### Module: `gc::marker`

#### `ParallelMarkConfig`

```rust
/// Configuration for parallel marking
#[derive(Debug, Clone, Copy)]
pub struct ParallelMarkConfig {
    /// Maximum number of parallel marking workers
    /// Default: min(num_cpus, 16)
    pub max_workers: usize,

    /// Per-queue capacity
    pub queue_capacity: usize,

    /// Enable parallel Minor GC
    pub parallel_minor_gc: bool,

    /// Enable parallel Major GC
    pub parallel_major_gc: bool,
}

impl Default for ParallelMarkConfig {
    fn default() -> Self {
        Self {
            max_workers: std::cmp::min(num_cpus::get(), 16),
            queue_capacity: 1024,
            parallel_minor_gc: true,
            parallel_major_gc: true,
        }
    }
}
```

#### `ParallelMarkCoordinator`

```rust
pub struct ParallelMarkCoordinator {
    queues: Vec<PerThreadMarkQueue>,
    barrier: std::sync::Barrier,
    page_to_queue: std::collections::HashMap<usize, usize>,
    total_marked: std::sync::atomic::AtomicUsize,
}

impl ParallelMarkCoordinator {
    /// Create a new coordinator with the specified number of workers
    pub fn new(num_workers: usize) -> Self;

    /// Register pages for a specific queue (worker)
    pub fn register_pages(
        &mut self,
        queue_idx: usize,
        pages: &[std::ptr::NonNull<PageHeader>],
    );

    /// Distribute stack roots to appropriate work queues
    pub fn distribute_roots(
        &self,
        roots: impl Iterator<Item = (*const u8, std::sync::Arc<ThreadControlBlock>)>,
        find_gc_box: impl Fn(*const u8) -> Option<std::ptr::NonNull<GcBox<()>>>,
    );

    /// Distribute dirty pages for Minor GC
    pub fn distribute_dirty_pages(&self, heap: &LocalHeap);

    /// Execute parallel marking
    ///
    /// Returns the total number of objects marked.
    pub fn mark(
        &self,
        heap: &LocalHeap,
        kind: VisitorKind,
    ) -> usize;

    /// Get the number of workers
    #[must_use]
    pub fn num_workers(&self) -> usize;
}
```

#### `PerThreadMarkQueue`

```rust
pub struct PerThreadMarkQueue {
    local_queue: Worklist<std::ptr::NonNull<GcBox<()>>>,
    steal_queue:
        StealQueue<std::ptr::NonNull<GcBox<()>>, 1024>,
    owned_pages: Vec<std::ptr::NonNull<PageHeader>>,
    marked_count: std::sync::atomic::AtomicUsize,
    thread_id: std::thread::ThreadId,
}

impl PerThreadMarkQueue {
    /// Create a new per-thread mark queue
    pub fn new(thread_id: std::thread::ThreadId) -> Self;

    /// Push to local queue (LIFO, fastest path)
    pub fn push_local(&mut self, ptr: std::ptr::NonNull<GcBox<()>>);

    /// Pop from local queue
    pub fn pop_local(&mut self) -> Option<std::ptr::NonNull<GcBox<()>>>;

    /// Steal from this queue (called by other threads)
    pub fn steal(&self) -> Option<std::ptr::NonNull<GcBox<()>>>;

    /// Process all objects on an owned page
    pub fn process_owned_page(
        &mut self,
        page: std::ptr::NonNull<PageHeader>,
    );

    /// Get the marked count
    #[must_use]
    pub fn marked_count(&self) -> usize;

    /// Get the thread ID
    #[must_use]
    pub fn thread_id(&self) -> std::thread::ThreadId;
}
```

---

## Internal API (for integration)

### Modified: `heap.rs`

#### PageHeader additions

```rust
impl PageHeader {
    /// Atomically try to mark an object (CAS-based)
    ///
    /// Returns `true` if successfully marked (or already marked).
    #[inline]
    pub fn try_mark(&self, index: usize) -> bool;

    /// Check if all objects in this page are marked
    #[must_use]
    pub fn is_fully_marked(&self) -> bool;
}
```

---

## Usage Examples

### Example 1: Basic Parallel Marking

```rust
use rudo_gc::{collect, Gc, Trace};

#[derive(Trace)]
struct Node {
    value: i32,
    next: Option<Gc<Node>>,
}

fn main() {
    // Create a large graph of nodes
    let mut nodes: Vec<Gc<Node>> = Vec::new();
    for i in 0..100_000 {
        nodes.push(Gc::new(Node {
            value: i,
            next: if i > 0 {
                Some(Gc::clone(&nodes[i - 1]))
            } else {
                None
            },
        }));
    }

    // Trigger GC - parallel marking will run
    collect();

    // Verify nodes are still accessible
    assert_eq!(nodes[99_999].value, 0);
}
```

### Example 2: Configure Worker Count

```rust
use rudo_gc::gc::marker::ParallelMarkConfig;

fn main() {
    // Configure parallel marking
    let config = ParallelMarkConfig {
        max_workers: 4,  // Use 4 workers
        queue_capacity: 2048,
        parallel_minor_gc: true,
        parallel_major_gc: true,
    };

    // Apply configuration (hypothetical API)
    rudo_gc::gc::marker::set_parallel_config(config);
}
```

---

## Error Handling

All parallel marking operations use the following error handling strategy:

| Operation | Error Type | Recovery |
|-----------|------------|----------|
| Queue full | `None` from `push()` | Retry with larger queue or different strategy |
| Queue empty | `None` from `pop()`/`steal()` | Normal termination condition |
| CAS failure | Loop retry | Automatic in `try_mark()` |
| Page not found | `None` from routing | Object added to local queue |

---

## Thread Safety Guarantees

| Type | Send | Sync |
|------|------|------|
| `StealQueue<T, N>` | T: Send | Yes |
| `PerThreadMarkQueue` | Yes | Yes |
| `ParallelMarkCoordinator` | No | Yes |
| `ParallelMarkConfig` | Yes | Yes |
