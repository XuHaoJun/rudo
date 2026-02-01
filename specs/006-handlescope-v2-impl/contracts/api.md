# API Contracts: HandleScope v2

**Feature**: HandleScope v2 Implementation | **Date**: 2026-02-01

## Public API

### Core Types

```rust
// HandleScope - RAII scope for handle management
pub struct HandleScope<'env>;

// Handle - Lifetime-bound GC reference
pub struct Handle<'scope, T: Trace>;

// EscapeableHandleScope - Allows single handle escape
pub struct EscapeableHandleScope<'env>;

// SealedHandleScope - Debug-only handle prevention
pub struct SealedHandleScope<'env>;

// MaybeHandle - Optional handle with better layout
pub struct MaybeHandle<'scope, T: Trace>;

// AsyncHandleScope - Async/await safe scope
pub struct AsyncHandleScope;

// AsyncHandle - Handle without lifetime parameter
pub struct AsyncHandle<T: Trace>;

// Internal types (public for advanced use)
pub struct LocalHandles;
pub struct HandleBlock;
pub struct HandleSlot;
pub struct HandleScopeData;
pub struct AsyncScopeData;
pub struct AsyncScopeEntry;
```

### Constants

```rust
/// Number of slots per HandleBlock
pub const HANDLE_BLOCK_SIZE: usize = 256;
```

## Function Contracts

### HandleScope::new

```rust
impl<'env> HandleScope<'env> {
    pub fn new(tcb: &'env ThreadControlBlock) -> Self;
}
```

**Preconditions**:
- `tcb` must be a valid shared reference to `ThreadControlBlock`

**Postconditions**:
- Returns a new `HandleScope` bound to `'env`
- Scope level incremented by 1

**Panics**: Never (debug: panics in SealedHandleScope)

**Thread Safety**: Single-threaded (Handle is !Send + !Sync)

**Design Note**: Uses shared `&ThreadControlBlock` to allow nested scopes

---

### HandleScope::handle

```rust
impl<'env> HandleScope<'env> {
    pub fn handle<'scope, T: Trace>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T>;
}
```

**Preconditions**:
- `gc` must be a valid `Gc<T>`
- `self` must be an active `HandleScope`

**Postconditions**:
- Returns `Handle<'scope, T>` where `'scope` is `self`'s lifetime
- Handle will be invalidated when `self` drops

**Panics**: Never (block expansion handles allocation)

**Thread Safety**: Single-threaded (non-Sync Handle)

---

### Handle::get

```rust
impl<'scope, T: Trace> Handle<'scope, T> {
    pub fn get(&self) -> &T;
}
```

**Preconditions**:
- `self` must be a valid handle from an active `HandleScope`
- Handle slot must contain valid `GcBox` pointer

**Postconditions**:
- Returns `&T` to the GC'd value

**Panics**: Never (validity guaranteed by lifetime)

**Thread Safety**: Single-threaded (non-Sync Handle)

---

### Handle::to_gc

```rust
impl<'scope, T: Trace> Handle<'scope, T> {
    pub fn to_gc(&self) -> Gc<T>;
}
```

**Preconditions**:
- `self` must be a valid handle

**Postconditions**:
- Returns new `Gc<T>` with incremented reference count
- Original handle remains valid

**Panics**: Never

**Thread Safety**: Single-threaded

---

### Handle::as_ptr

```rust
impl<'scope, T: Trace> Handle<'scope, T> {
    pub unsafe fn as_ptr(&self) -> *const GcBox<T>;
}
```

**Preconditions**:
- `self` must be a valid handle

**Postconditions**:
- Returns raw pointer to internal `GcBox`
- Pointer valid only while handle is valid

**Panics**: Never

**Safety**: Caller must ensure handle validity

---

### EscapeableHandleScope::new

```rust
impl<'env> EscapeableHandleScope<'env> {
    pub fn new(tcb: &'env ThreadControlBlock) -> Self;
}
```

**Preconditions**:
- `tcb` must be valid shared reference

**Postconditions**:
- Creates `EscapeableHandleScope` with escaped=false
- Pre-allocates escape slot in parent scope
- Debug: Records parent_level for validation

**Panics**: Never

---

### EscapeableHandleScope::escape

```rust
impl<'env> EscapeableHandleScope<'env> {
    pub fn escape<'parent, T: Trace>(
        &self,
        parent: &'parent HandleScope<'_>,
        handle: Handle<'_, T>,
    ) -> Handle<'parent, T>;
}
```

**Preconditions**:
- `self` must not have called escape before
- `handle` must be from this scope  
- `parent` must be the actual parent scope

**Postconditions**:
- Returns `Handle<'parent, T>` with lifetime bound to parent scope
- Sets escaped=true
- Copies handle content to pre-allocated escape slot

**Panics**:
- If escape already called
- Debug: If parent level doesn't match expected

**Design Note**: Requires `parent` parameter to constrain returned Handle lifetime safely

---

### EscapeableHandleScope::handle

```rust
impl<'env> EscapeableHandleScope<'env> {
    pub fn handle<'scope, T: Trace>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T>;
}
```

**Preconditions**: Same as `HandleScope::handle`

**Postconditions**: Creates handle in inner scope

**Panics**: Never

---

### SealedHandleScope::new

```rust
impl<'env> SealedHandleScope<'env> {
    pub fn new(tcb: &'env ThreadControlBlock) -> Self;
}
```

**Preconditions**: `tcb` must be valid shared reference

**Postconditions**:
- Debug: Sets `sealed_level = level` to prevent allocations at current level
- Release: Returns zero-sized `PhantomData` wrapper (no runtime cost)

**Panics**: Never

**Design Note**: Uses `sealed_level` field (V8 pattern) rather than `limit` manipulation

---

### MaybeHandle::empty

```rust
impl<'scope, T: Trace> MaybeHandle<'scope, T> {
    pub const fn empty() -> Self;
}
```

**Postconditions**:
- Returns empty `MaybeHandle` with null slot

---

### MaybeHandle::from_handle

```rust
impl<'scope, T: Trace> MaybeHandle<'scope, T> {
    pub fn from_handle(handle: Handle<'scope, T>) -> Self;
}
```

**Postconditions**:
- Returns `MaybeHandle` containing handle's slot

---

### MaybeHandle::is_empty

```rust
impl<'scope, T: Trace> MaybeHandle<'scope, T> {
    pub fn is_empty(&self) -> bool;
}
```

**Postconditions**:
- Returns true if slot is null

---

### MaybeHandle::to_handle

```rust
impl<'scope, T: Trace> MaybeHandle<'scope, T> {
    pub fn to_handle(self) -> Option<Handle<'scope, T>>;
}
```

**Postconditions**:
- Returns Some(handle) if not empty, None otherwise

---

### AsyncHandleScope::new

```rust
impl AsyncHandleScope {
    pub fn new(tcb: &Arc<ThreadControlBlock>) -> Self;
}
```

**Preconditions**:
- `tcb` must be valid `Arc<ThreadControlBlock>`

**Postconditions**:
- Generates unique scope ID
- Registers scope ID and block pointer with TCB's async_scopes list
- Creates dedicated handle block

**Panics**: Never

**Thread Safety**: Send (uses Arc<TCB>)

---

### AsyncHandleScope::handle

```rust
impl AsyncHandleScope {
    pub fn handle<T: Trace>(&self, gc: &Gc<T>) -> AsyncHandle<T>;
}
```

**Preconditions**:
- `gc` must be valid
- Scope must not be dropped

**Postconditions**:
- Returns `AsyncHandle<T>` with scope_id set
- Increments atomic used counter

**Panics**: If more than HANDLE_BLOCK_SIZE handles created

---

### AsyncHandleScope::with_guard

```rust
impl AsyncHandleScope {
    pub fn with_guard<F, R>(&self, f: F) -> R
    where
        F: FnOnce(AsyncHandleGuard<'_>) -> R;
}
```

**Postconditions**:
- Calls closure with guard that provides safe handle access
- Guard lifetime tied to scope borrow

**Panics**: Never

**Design Note**: Preferred safe access pattern for AsyncHandle

---

### AsyncHandleScope::iterate

```rust
impl AsyncHandleScope {
    pub fn iterate(&self, visitor: &mut GcVisitor);
}
```

**Preconditions**:
- `visitor` must be valid

**Postconditions**:
- Visits all handles as GC roots (precise, not conservative)

**Thread Safety**: Called during stop-the-world GC

**Design Note**: Does not use `find_gc_box_from_ptr` - handles are already precise pointers

---

### AsyncHandleGuard::get

```rust
impl<'scope> AsyncHandleGuard<'scope> {
    pub fn get<T: Trace>(&self, handle: &AsyncHandle<T>) -> &T;
}
```

**Preconditions**:
- `handle` must belong to the same scope as guard

**Postconditions**:
- Returns `&T` with lifetime tied to guard

**Panics**: Debug: If handle.scope_id != guard.scope.id

**Design Note**: Safe access pattern - lifetime tied to scope borrow

---

### AsyncHandle::get

```rust
impl<T: Trace> AsyncHandle<T> {
    pub unsafe fn get(&self) -> &T;
}
```

**Preconditions**:
- Parent `AsyncHandleScope` must not be dropped

**Postconditions**:
- Returns `&T` to value

**Safety**: Caller must ensure scope is alive  
**Recommendation**: Use `scope.with_guard()` for safe access

---

### AsyncHandle::to_gc

```rust
impl<T: Trace> AsyncHandle<T> {
    pub fn to_gc(&self) -> Gc<T>;
}
```

**Postconditions**:
- Returns `Gc<T>` with incremented refcount
- Safe even if scope may drop later (Gc owns the reference)

---

### LocalHandles::new

```rust
impl LocalHandles {
    pub fn new() -> Self;
}
```

**Postconditions**:
- Returns empty LocalHandles with no blocks

---

### LocalHandles::scope_data_mut

```rust
impl LocalHandles {
    pub fn scope_data_mut(&mut self) -> &mut HandleScopeData;
}
```

**Postconditions**:
- Returns mutable reference to scope data

---

### LocalHandles::add_block

```rust
impl LocalHandles {
    pub fn add_block(&mut self) -> *mut HandleSlot;
}
```

**Postconditions**:
- Allocates new HandleBlock
- Links it into blocks list
- Returns pointer to first slot

---

### LocalHandles::allocate

```rust
impl LocalHandles {
    pub fn allocate(&mut self) -> *mut HandleSlot;
}
```

**Postconditions**:
- Returns pointer to allocated slot
- Increments next pointer

---

### LocalHandles::iterate

```rust
impl LocalHandles {
    pub fn iterate(&self, visitor: &mut GcVisitor);
}
```

**Postconditions**:
- Visits all handles as GC roots

---

## ThreadControlBlock Extensions

### local_handles_mut

```rust
impl ThreadControlBlock {
    pub fn local_handles_mut(&mut self) -> &mut LocalHandles;
}
```

**Postconditions**:
- Returns mutable reference to local handles

---

### register_async_scope

```rust
impl ThreadControlBlock {
    pub fn register_async_scope(&self, id: u64, data: Arc<AsyncScopeData>);
}
```

**Preconditions**:
- `id` must be a unique scope identifier generated by `AsyncHandleScope::new()`
- `data` must be a valid `Arc` to `AsyncScopeData` owned by the registering `AsyncHandleScope`

**Postconditions**:
- Adds scope entry to async_scopes list
- The `Arc` is cloned, so `AsyncHandleScope` and TCB share ownership

**Thread Safety**: Safe to call from any thread; uses internal Mutex

**Design Note**: Uses `Arc<AsyncScopeData>` instead of raw pointers to ensure data remains valid regardless of drop ordering between `AsyncHandleScope` and GC registration.

---

### unregister_async_scope

```rust
impl ThreadControlBlock {
    pub fn unregister_async_scope(&self, id: u64);
}
```

**Preconditions**:
- `id` must be a scope identifier previously registered with `register_async_scope`

**Postconditions**:
- Removes scope entry from async_scopes list
- The `Arc` reference count is decremented

**Thread Safety**: Safe to call from any thread; uses internal Mutex

---

### iterate_all_handles

```rust
impl ThreadControlBlock {
    pub fn iterate_all_handles(&self, heap: &LocalHeap, visitor: &mut GcVisitor);
}
```

**Postconditions**:
- Visits all sync and async handles as roots

---

## Macro Contracts

### spawn_with_gc!

```rust
#[macro_export]
macro_rules! spawn_with_gc {
    // Single Gc
    ($gc:expr => |$handle:ident| $body:expr) => { ... };

    // Multiple Gc
    ($($gc:ident),+ => |$($handle:ident),+| $body:expr) => { ... };
}
```

**Preconditions**:
- Called within GC thread context
- `$gc` must be `Gc<T>` values
- `$body` must be async block

**Postconditions**:
- Spawns task with automatic GC root tracking
- Handle(s) valid throughout task

**Panics**: If not in GC thread context

---

## Trait Implementations

### Handle<'scope, T>

| Trait | Implementation |
|-------|----------------|
| `Deref` | Dereferences to `&T` |
| `Copy` | Handle is copyable |
| `Clone` | Same as copy |
| `!Send` | Thread-local by design |
| `!Sync` | Thread-local by design |
| `Debug` | Delegates to `T::Debug` if implemented |
| `Display` | Delegates to `T::Display` if implemented |

### AsyncHandle<T>

| Trait | Implementation |
|-------|----------------|
| `Copy` | AsyncHandle is copyable |
| `Send` | Safe to send between threads |
| `Sync` | Safe to share between threads |

---

## Error Conditions

| Condition | Behavior |
|-----------|----------|
| Double escape | `panic!` with message |
| Block overflow in AsyncHandleScope | `panic!` with message |
| Use after AsyncHandleScope drop | Undefined behavior |
| Invalid Gc pointer | Undefined behavior |
| Non-GC-thread context for spawn_with_gc! | `panic!` with expect |

---

## Stability Guarantees

| API | Stability |
|-----|-----------|
| `HandleScope`, `Handle` | Stable |
| `EscapeableHandleScope` | Stable |
| `SealedHandleScope` | Stable (debug-only) |
| `MaybeHandle` | Stable |
| `AsyncHandleScope` | Stable (requires `async` feature) |
| `AsyncHandle` | Stable (requires `async` feature) |
| `spawn_with_gc!` | Stable (requires `async` feature) |
| `LocalHandles`, `HandleBlock`, `HandleSlot` | Internal (may change) |
| `HandleScopeData` | Internal (may change) |
