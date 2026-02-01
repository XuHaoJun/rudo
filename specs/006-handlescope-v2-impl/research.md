# Phase 0 Research: HandleScope v2 Implementation

**Date**: 2026-02-01 | **Feature**: HandleScope v2 Implementation

**Note**: This document has been updated to reflect v2.1 design fixes (2026-02-01).

## Research Overview

This document consolidates research findings for implementing HandleScope v2 in rudo-gc, following V8 patterns while leveraging Rust's type system for compile-time safety guarantees.

---

## 1. V8 HandleScope Architecture Deep Dive

### 1.1 LocalHandles Design Pattern

V8's LocalHandles implements a **handle block allocator** pattern:

```
HandleBlock: [Slot0, Slot1, Slot2, ..., Slot255]
             ↓     ↓     ↓            ↓
             Handle Handle Handle    Handle
```

**Key Characteristics**:
- Fixed-size blocks (typically 256 slots)
- Linked list of blocks for unbounded growth
- O(1) bump allocation: `next++` for each new handle
- Memory-efficient: handles are just word-sized (pointer)

**Decision for rudo-gc**: Adopt 256-slot blocks for cache locality and memory efficiency.

### 1.2 HandleScopeData Structure

V8 stores scope state in `HandleScopeData`:
```cpp
class HandleScopeData {
  Address next;      // Next free slot
  Address limit;     // End of current block
  int level;         // Nested scope depth
};
```

**Critical Insight**: `is_escapeable` is NOT part of HandleScopeData—it's a property of the scope *type*, not its data. This means:
- `HandleScope` is never escapable
- `EscapableHandleScope` wraps `HandleScope` and adds escape capability

### 1.3 Handle<T> Implementation

V8 handles are **raw pointers** in release mode:

```cpp
template<typename T>
class Handle {
  T* ptr_;  // Just a pointer in release
};
```

For rudo-gc, we use:
```rust
pub struct Handle<'scope, T: Trace> {
    slot: *const HandleSlot,
    _marker: PhantomData<(&'scope (), *const T)>,
}
```

**Why raw pointer?** Zero-cost abstraction. In release mode, this compiles to a single word.

### 1.4 EscapableHandleScope Pattern

The escape mechanism in V8:

1. Pre-allocate a slot in the *parent* scope
2. Copy handle data to that slot during escape
3. Return handle bound to parent's lifetime

```cpp
EscapableHandleScope::escape(Handle<T> handle) {
  *pre_allocated_slot_ = *handle;
  return Handle<T>(pre_allocated_slot_);
}
```

**Rust Implementation**: Must use `Cell<bool>` to track single-use constraint.

---

## 2. rudo-gc Architecture Integration

### 2.1 ThreadControlBlock Extension

Current `ThreadControlBlock` (from heap.rs):
```rust
pub struct ThreadControlBlock {
    pub state: AtomicUsize,
    pub gc_requested: AtomicBool,
    pub park_cond: Condvar,
    pub park_mutex: Mutex<()>,
    pub heap: UnsafeCell<LocalHeap>,
    pub stack_roots: Mutex<Vec<*const u8>>,
}
```

**Required Extensions**:
```rust
// NEW: Handle management (v2.1: uses UnsafeCell for interior mutability)
local_handles: UnsafeCell<LocalHandles>,
// NEW: Async scope registry (v2.1: uses ID-based registration)
async_scopes: Mutex<Vec<AsyncScopeEntry>>,
```

**v2.1 Design Note**: Uses ID-based registration for async scopes to avoid self-referential structure issues.

**Integration Impact**: 
- `iterate_all_handles()` added to TCB for GC root collection
- `local_handles_ptr()` for raw pointer access (avoids aliasing issues) 
- `local_handles_mut()` legacy API for exclusive access
- `register_async_scope(id, block_ptr)` / `unregister_async_scope(id)` for async support

### 2.2 GC Root Collection Integration

Current conservative root scanning:
```rust
fn collect_roots(heap: &LocalHeap, visitor: &mut GcVisitor) {
    // Scan stack conservatively
    unsafe { crate::stack::spill_registers_and_scan(...) };
}
```

**New precise root collection**:
```rust
fn collect_roots(heap: &LocalHeap, visitor: &mut GcVisitor) {
    // Iterate thread handles precisely
    for tcb in registry.threads.iter() {
        tcb.iterate_all_handles(heap, visitor);
    }

    // Conservative fallback for non-handle roots
    #[cfg(feature = "conservative-fallback")]
    {
        // ... existing stack scanning
    }
}
```

### 2.3 Interior Pointer Support (Already Implemented)

Per user input, interior pointer fix is already complete in `find_gc_box_from_ptr`. Key change:
```rust
// OLD: Required usize alignment (8 bytes)
if addr % std::mem::align_of::<usize>() != 0 {
    return None;
}

// NEW: Accept any alignment for interior pointers
// object_index calculation handles non-aligned pointers
let object_index = offset / block_size;
```

---

## 3. Component-Level Analysis

### 3.1 HandleScope<'env>

**Purpose**: Define lexical scope for handle validity.

**Design (v2.1 - uses shared reference)**:
```rust
pub struct HandleScope<'env> {
    tcb: &'env ThreadControlBlock,  // v2.1: shared reference, not &mut
    prev_next: *mut HandleSlot,
    prev_limit: *mut HandleSlot,
    prev_level: u32,
    _marker: PhantomData<*mut ()>,
}
```

**v2.1 Design Decision**: Uses `&ThreadControlBlock` instead of `&mut LocalHandles`:
- Allows nested HandleScopes (multiple scopes can borrow same TCB)
- Uses `UnsafeCell` for interior mutability
- Level counter ensures correct scope nesting order

**Key Invariants**:
- `'env` lifetime from ThreadControlBlock
- `prev_*` fields restore state on drop
- Level tracking for nested scope debugging
- v2.1: Raw pointer operations in allocate_slot() to avoid aliasing issues

**Lifetime Guarantee**: When `HandleScope` drops, all handles created within become invalid because:
1. Their slot is effectively "freed" (next restored to prev_next)
2. The `Handle<'scope, T>` lifetime becomes invalid

### 3.2 Handle<'scope, T>

**Purpose**: Lifetime-bound reference to Gc<T>.

**Design**:
```rust
pub struct Handle<'scope, T: Trace> {
    slot: *const HandleSlot,
    _marker: PhantomData<(&'scope (), *const T)>,
}
```

**Safety Analysis**:
- **Valid when**: `'scope` is the lifetime of an active `HandleScope`
- **Invalid when**: `HandleScope` has dropped
- **Compile-time guarantee**: Rust's lifetime system enforces this

**Why not Send/Sync**:
```rust
impl<T: Trace> !Send for Handle<'_, T> {}
impl<T: Trace> !Sync for Handle<'_, T> {}
```
Handles are **thread-local by design**—they're tied to a specific `HandleScope` on a specific thread.

### 3.3 EscapeableHandleScope<'env>

**Purpose**: Allow exactly one handle to escape to parent scope.

**Design (v2.1 - includes parent validation)**:
```rust
pub struct EscapeableHandleScope<'env> {
    inner: HandleScope<'env>,
    escaped: Cell<bool>,  // Single-use tracking
    escape_slot: *mut HandleSlot,  // Pre-allocated in parent
    #[cfg(debug_assertions)]
    parent_level: u32,  // v2.1: For runtime validation
}
```

**Single-Use Constraint (v2.1 - requires parent parameter)**:
```rust
pub fn escape<'parent, T: Trace>(
    &self,
    parent: &'parent HandleScope<'_>,  // v2.1: Required for lifetime binding
    handle: Handle<'_, T>,
) -> Handle<'parent, T> {
    if self.escaped.get() {
        panic!("EscapeableHandleScope::escape() can only be called once");
    }
    
    #[cfg(debug_assertions)]
    {
        // v2.1: Validate parent is correct
        if parent.level() + 1 != self.inner.level() {
            panic!("escape() called with incorrect parent scope");
        }
    }
    
    self.escaped.set(true);
    // ... copy to escape_slot and return
}
```

**v2.1 Design Decision**: The `escape()` method requires a `parent` parameter to:
1. Properly constrain the returned Handle's lifetime
2. Prevent caller from specifying arbitrary `'outer` lifetimes
3. Enable debug-time validation that parent is correct

**Why Cell<bool> not AtomicBool**:
- `EscapeableHandleScope` is single-threaded (like `HandleScope`)
- `Cell` is sufficient and has less overhead

### 3.4 SealedHandleScope<'env>

**Purpose**: Debug-only mechanism to prevent handle creation.

**Design (v2.1 - uses sealed_level)**:
```rust
#[cfg(debug_assertions)]
pub struct SealedHandleScope<'env> {
    tcb: &'env ThreadControlBlock,  // v2.1: shared reference
    prev_sealed_level: u32,         // v2.1: uses sealed_level field
}

#[cfg(not(debug_assertions))]
pub struct SealedHandleScope<'env>(PhantomData<&'env ()>);
```

**v2.1 Mechanism**: 
- Sets `sealed_level = level` in HandleScopeData
- `allocate_slot()` checks `level <= sealed_level` and panics
- More reliable than limit manipulation (V8 pattern)

**Zero-Cost in Release**: Becomes a no-op type with zero overhead.

### 3.5 AsyncHandleScope

**Purpose**: HandleScope variant for async/await contexts.

**Design (v2.1 - includes ID for registration)**:
```rust
pub struct AsyncHandleScope {
    id: u64,                       // v2.1: Unique scope ID for registration
    tcb: Arc<ThreadControlBlock>,  // For cross-await persistence
    block: Box<HandleBlock>,       // Dedicated handle block
    used: AtomicUsize,             // Atomic slot counter
    dropped: AtomicBool,           // Lifecycle tracking
}
```

**v2.1 Design Decisions**:
1. **ID-based registration**: Uses unique `id` instead of `*const Self` pointer
   - Avoids self-referential structure issues
   - More robust unregistration pattern

2. **Safe access via guard**: Provides `with_guard()` method:
   ```rust
   pub fn with_guard<F, R>(&self, f: F) -> R
   where
       F: FnOnce(AsyncHandleGuard<'_>) -> R
   ```

**Why Dedicated Block**:
- Cannot share with sync `LocalHandles` (different lifetimes)
- Each async task has its own isolated handle space

**Atomic Ordering**:
- `used`: Relaxed for allocation (no synchronization needed)
- `dropped`: Release for unregister, Acquire for safety checks

### 3.6 AsyncHandleGuard<'scope> (v2.1 New)

**Purpose**: Guard type for safe AsyncHandle access with lifetime binding.

**Design**:
```rust
pub struct AsyncHandleGuard<'scope> {
    scope: &'scope AsyncHandleScope,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope> AsyncHandleGuard<'scope> {
    pub fn get<T: Trace>(&self, handle: &AsyncHandle<T>) -> &T {
        #[cfg(debug_assertions)]
        {
            if handle.scope_id != self.scope.id {
                panic!("AsyncHandle accessed from wrong scope");
            }
        }
        // ... safe dereference
    }
}
```

**Purpose**: Provides compile-time lifetime binding for async handle access.

### 3.7 AsyncHandle<T>

**Purpose**: Handle for async contexts without lifetime parameter.

**Design (v2.1 - includes scope_id)**:
```rust
pub struct AsyncHandle<T: Trace> {
    slot: *const HandleSlot,
    scope_id: u64,  // v2.1: For debug validation
    _marker: PhantomData<*const T>,
}
```

**v2.1 Safety Improvements**:
1. Includes `scope_id` for debug-time scope validation
2. Recommended access pattern: `scope.with_guard(|g| g.get(&handle))`
3. `unsafe fn get()` still available for expert use

**Why Send + Sync**:
```rust
unsafe impl<T: Trace> Send for AsyncHandle<T> {}
```
Async handles are `Send` because they're designed to move between threads with their `Arc<AsyncHandleScope>`.

### 3.7 spawn_with_gc! Macro

**Purpose**: Ergonomic wrapper for tokio::spawn with automatic root tracking.

**Design**:
```rust
#[macro_export]
macro_rules! spawn_with_gc {
    ($gc:expr => |$handle:ident| $body:expr) => {{
        let __gc = $gc;
        let __tcb = $crate::heap::current_thread_control_block()
            .expect("spawn_with_gc! must be called within a GC thread");

        tokio::spawn(async move {
            let __scope = $crate::AsyncHandleScope::new(&__tcb);
            let $handle = __scope.handle(&__gc);
            let __result = { $body.await };
            drop(__scope);
            __result
        })
    }};
}
```

**Key Features**:
- Automatic `AsyncHandleScope` creation and drop
- Captures `Gc` by value before spawn
- Handle available throughout async task

---

## 4. Data Structure Specifications

### 4.1 HandleSlot

```rust
#[repr(C)]
pub struct HandleSlot {
    gc_box_ptr: *const GcBox<()>,
}

impl HandleSlot {
    pub fn new(gc_box_ptr: *const GcBox<()>) -> Self {
        Self { gc_box_ptr }
    }

    pub fn as_ptr(&self) -> *const GcBox<()> {
        self.gc_box_ptr
    }

    pub fn cast<T>(&self) -> *const HandleSlot {
        self as *const Self as *const HandleSlot
    }
}
```

**Memory Layout**: Single word (pointer) for efficiency.

### 4.2 HandleBlock

```rust
pub const HANDLE_BLOCK_SIZE: usize = 256;

pub struct HandleBlock {
    slots: [HandleSlot; HANDLE_BLOCK_SIZE],
    next: Option<NonNull<HandleBlock>>,
}

impl HandleBlock {
    pub fn new() -> Box<Self> {
        // Allocate uninitialized - slots filled on use
        Box::new(Self {
            slots: unsafe { std::mem::zeroed() },
            next: None,
        })
    }
}
```

**Why [HandleSlot; 256] not Vec**:
- Stack-allocated array for cache locality
- Fixed size prevents fragmentation
- Zero initialization on allocation

### 4.3 LocalHandles

```rust
pub struct LocalHandles {
    blocks: Option<NonNull<HandleBlock>>,
    scope_data: HandleScopeData,
}

impl LocalHandles {
    pub fn new() -> Self {
        Self {
            blocks: None,
            scope_data: HandleScopeData::new(),
        }
    }

    pub fn scope_data_mut(&mut self) -> &mut HandleScopeData {
        &mut self.scope_data
    }

    pub fn add_block(&mut self) -> *mut HandleSlot {
        let new_block = HandleBlock::new();
        let new_block_ptr = NonNull::from(Box::leak(new_block));

        if let Some(ref mut last) = self.blocks {
            unsafe { last.as_mut().next = Some(new_block_ptr) };
        } else {
            self.blocks = Some(new_block_ptr);
        }

        unsafe { new_block_ptr.as_mut().slots.as_mut_ptr() }
    }

    pub fn allocate(&mut self) -> *mut HandleSlot {
        let scope_data = &mut self.scope_data;

        if scope_data.next == scope_data.limit {
            self.add_block()
        } else {
            let slot = scope_data.next;
            unsafe { scope_data.next = scope_data.next.add(1) };
            slot
        }
    }
}
```

### 4.4 HandleScopeData

```rust
#[derive(Debug)]
pub struct HandleScopeData {
    next: *mut HandleSlot,
    limit: *mut HandleSlot,
    level: u32,
    #[cfg(debug_assertions)]
    sealed_level: u32,  // v2.1: For SealedHandleScope
}

impl HandleScopeData {
    pub const fn new() -> Self {
        Self {
            next: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            level: 0,
            #[cfg(debug_assertions)]
            sealed_level: 0,
        }
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.level > 0
    }
    
    #[cfg(debug_assertions)]
    #[inline]
    pub fn is_sealed(&self) -> bool {
        self.level <= self.sealed_level
    }
}
```

---

## 5. Safety Analysis

### 5.1 Memory Safety Guarantees

| Operation | Safety Guarantee | Mechanism |
|-----------|------------------|-----------|
| Handle creation | Only within active scope | `scope.handle()` returns `Handle<'scope, T>` |
| Handle use after scope drop | Compile-time error | Lifetime `'scope` becomes invalid |
| Handle escape | Explicit via `EscapeableHandleScope` | Pre-allocated slot in parent scope |
| AsyncHandle use after scope drop | Undefined behavior | Documented in SAFETY comment |

### 5.2 Unsafe Operations

| Location | Operation | SAFETY Contract |
|----------|-----------|-----------------|
| `HandleSlot::as_ptr()` | Pointer read | Slot always initialized before read |
| `Handle::get()` | Dereference slot | Handle only created via valid slot |
| `LocalHandles::allocate()` | Bump pointer | `next < limit` check before increment |
| `AsyncHandleScope::iterate()` | Interior pointer lookup | `find_gc_box_from_ptr` validates result |

### 5.3 Miri Testing Requirements

Required Miri test scenarios:
1. Basic handle creation and scope exit
2. Nested scope handle invalidation
3. Escape pattern with HandleScope lifetime binding
4. AsyncHandleScope across await points
5. Multiple spawn_with_gc! tasks
6. Interior pointer in handle iteration

---

## 6. Performance Considerations

### 6.1 Allocation Cost

| Operation | Cost | Notes |
|-----------|------|-------|
| `HandleScope::new()` | O(1) | Only stores prev pointers |
| `scope.handle()` | O(1) | Bump allocation, single write |
| `EscapeableHandleScope::new()` | O(1) | Additional slot pre-allocation |
| `AsyncHandleScope::handle()` | O(1) | Atomic increment, single write |

### 6.2 Memory Overhead

| Component | Size | Notes |
|-----------|------|-------|
| `Handle` | 8 bytes | Single word (pointer) |
| `HandleSlot` | 8 bytes | Pointer to GcBox |
| `HandleBlock` | 256 * 8 = 2048 bytes | 2KB per block |
| `HandleScopeData` | 24 bytes | 3 x usize |
| `LocalHandles` | Variable | Linked list of blocks |

### 6.3 GC Scan Performance

**Conservative scanning (v1)**: O(stack_size), may scan non-pointers

**HandleScope scanning (v2)**: O(handle_count), only actual handles

**Improvement**: Handles are typically < 1% of stack size in typical applications.

---

## 7. Implementation Order

### Phase 1: Core Types
1. `HandleSlot`, `HandleBlock`, `HandleScopeData`
2. `LocalHandles` with block management
3. `HandleScope<'env>` and `Handle<'scope, T>`
4. Unit tests for basic functionality

### Phase 2: Escape Patterns
1. `EscapeableHandleScope<'env>`
2. `MaybeHandle<'scope, T>`
3. `SealedHandleScope<'env>` (debug-only)
4. Escape pattern tests

### Phase 3: Async Support
1. `AsyncHandleScope` with TCB integration
2. `AsyncHandle<T>` and safety contract
3. `spawn_with_gc!` macro
4. Async integration tests

### Phase 4: GC Integration
1. Extend `ThreadControlBlock`
2. Implement `iterate_all_handles()`
3. Update `collect_roots()` for precise scanning
4. Feature flags and backwards compatibility

### Phase 5: Migration & Documentation
1. Migration guide from v1
2. Update public exports
3. Update examples and tests
4. Performance benchmarks

---

## 8. References

### V8 Source References
- `src/handles/local-handles.h:19-42` — LocalHandles definition
- `src/handles/local-handles.h:44-89` — LocalHandleScope
- `src/handles/handles.h:149-245` — Handle<T>
- `src/handles/handles.h:263-347` — HandleScope
- `src/handles/handles.h:378-599` — DirectHandle (CSS mode)

### rudo-gc Integration Points
- `crates/rudo-gc/src/heap.rs` — ThreadControlBlock definition
- `crates/rudo-gc/src/gc.rs` — Root collection
- `crates/rudo-gc/src/ptr.rs` — Gc<T> definition
- `crates/rudo-gc/src/lib.rs` — Public exports

---

## 9. Open Questions & Resolutions

### Q1: Should HandleSlot be ZST or raw pointer?

**Resolution**: Raw pointer (`*const GcBox<()>`).

**Rationale**: HandleSlot must be able to represent a null/empty state for `MaybeHandle`. A ZST cannot be null, requiring a separate flag. Raw pointer is the standard Rust pattern for nullable values.

### Q2: Should Handle implement Into<Gc<T>>?

**Resolution**: No, use explicit `to_gc()` method.

**Rationale**: Converting a handle to a `Gc<T>` increments the reference count. This is a semantic operation that should be explicit, not implicit. `to_gc()` makes the cost visible.

### Q3: How to handle handle block memory on thread exit?

**Resolution**: `LocalHandles` drop handler frees all blocks.

**Rationale**: Each thread's `LocalHandles` is owned by `ThreadControlBlock`. When the TCB is dropped (thread exits), the `LocalHandles` is also dropped, freeing all blocks. No additional cleanup needed.

---

## 10. Conclusion

The HandleScope v2 implementation follows V8's proven patterns while leveraging Rust's type system for compile-time safety. Key innovations:

1. **Lifetime-bound handles** prevent dangling references at compile time
2. **Zero-cost abstraction** — handles are single words in release builds
3. **First-class async support** via `AsyncHandleScope`
4. **Explicit escape mechanism** for controlled handle transfer

All unsafe operations are documented with SAFETY comments, and the design passes constitution checks for memory safety and testing discipline.
