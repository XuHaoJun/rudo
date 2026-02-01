# Phase 1: Data Model

**Feature**: HandleScope v2 Implementation | **Date**: 2026-02-01

## Entity Definitions

### Core Entities

#### HandleScope<'env>

**Description**: RAII-style scope that defines handle validity boundaries.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `tcb` | `&'env ThreadControlBlock` | Shared reference to TCB (allows nesting) |
| `prev_next` | `*mut HandleSlot` | Saved next pointer for restoration |
| `prev_limit` | `*mut HandleSlot` | Saved limit pointer for restoration |
| `prev_level` | `u32` | Saved scope level for nesting |
| `_marker` | `PhantomData<*mut ()>` | Prevents Send/Sync |

**Lifetime**: `'env` is the lifetime of the ThreadControlBlock

**Design Note**: Uses shared `&ThreadControlBlock` (not `&mut`) to allow nested scopes.
Internal mutability via `UnsafeCell` with level counter ensuring correct ordering.

**Invariants**:
- `prev_next` and `prev_limit` must be valid before HandleScope creation
- `prev_level` must be less than u32::MAX (no overflow in normal use)

**State Transitions**:
```
INACTIVE -> (new HandleScope) -> ACTIVE
ACTIVE -> (drop) -> INACTIVE
```

#### Handle<'scope, T>

**Description**: Lifetime-bound reference to garbage-collected data.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `slot` | `*const HandleSlot` | Pointer to slot containing GcBox |
| `_marker` | `PhantomData<(&'scope (), *const T)>` | Lifetime binding and type |

**Lifetime**: `'scope` is the lifetime of the creating HandleScope

**Type Constraints**: `T: Trace`

**Relationships**:
- Created by `HandleScope::handle(&Gc<T>)`
- Dereferences to `&T`
- Can be converted to `Gc<T>` via `to_gc()`

#### EscapeableHandleScope<'env>

**Description**: HandleScope variant allowing one handle to escape to parent scope.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `inner` | `HandleScope<'env>` | Inner scope for handle creation |
| `escaped` | `Cell<bool>` | Tracks if escape was used |
| `escape_slot` | `*mut HandleSlot` | Pre-allocated slot in parent |
| `parent_level` (debug) | `u32` | Parent level for validation |

**State Machine**:
```
NOT_ESCAPED -> (escape called) -> ESCAPED
ESCAPED -> (escape called) -> panic!
```

**Constraint**: Exactly one escape allowed per instance

**Design Note**: `escape()` requires `parent: &'parent HandleScope` parameter to constrain returned handle lifetime safely

#### SealedHandleScope<'env>

**Description**: Debug-only scope that prevents handle creation.

**Fields (debug)**:
| Field | Type | Purpose |
|-------|------|---------|
| `tcb` | `&'env ThreadControlBlock` | TCB reference |
| `prev_sealed_level` | `u32` | Saved sealed_level for restoration |

**Fields (release)**: Zero-sized wrapper (`PhantomData`)

**Mechanism**: Sets `sealed_level = level` in HandleScopeData; allocation checks if `level <= sealed_level`

**Design Note**: Uses `sealed_level` field (V8 pattern) instead of manipulating `limit`

#### AsyncHandleScope

**Description**: HandleScope for async/await contexts.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `id` | `u64` | Unique scope ID for registration |
| `tcb` | `Arc<ThreadControlBlock>` | Thread reference for cross-await |
| `block` | `Box<HandleBlock>` | Dedicated handle block |
| `used` | `AtomicUsize` | Allocation counter |
| `dropped` | `AtomicBool` | Drop flag for safety |

**Thread Safety**: `Send` (uses Arc)

**Design Note**: Uses ID-based registration to avoid self-referential structure issues

#### AsyncHandleGuard<'scope>

**Description**: Guard for safe AsyncHandle access with lifetime binding.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `scope` | `&'scope AsyncHandleScope` | Reference to scope |
| `_marker` | `PhantomData<&'scope ()>` | Lifetime marker |

**Purpose**: Provides safe access pattern for AsyncHandle via `scope.with_guard()`

#### AsyncHandle<T>

**Description**: Handle for async contexts without lifetime parameter.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `slot` | `*const HandleSlot` | Slot pointer |
| `scope_id` | `u64` | Owning scope ID (for debug validation) |
| `_marker` | `PhantomData<*const T>` | Type marker |

**Safety Contract**: 
- Valid only while parent AsyncHandleScope exists
- Recommended: Use `scope.with_guard()` for safe access
- `unsafe fn get()` available for expert use

#### LocalHandles

**Description**: Per-thread handle storage manager.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `blocks` | `Option<NonNull<HandleBlock>>` | Linked list of blocks |
| `scope_data` | `HandleScopeData` | Current allocation state |

**Operations**:
- `allocate()`: O(1) bump allocation
- `add_block()`: O(1) block creation
- `iterate()`: O(1) per handle for GC

#### HandleBlock

**Description**: Fixed-size array of handle slots.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `slots` | `[HandleSlot; 256]` | Fixed-size slot array |
| `next` | `Option<NonNull<HandleBlock>>` | Next block in list |

**Size**: 256 slots = 2048 bytes (2KB)

#### HandleSlot

**Description**: Individual handle storage.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `gc_box_ptr` | `*const GcBox<()>` | Pointer to GcBox |

**Memory**: Single word (8 bytes on 64-bit)

#### HandleScopeData

**Description**: Runtime state for scope management.

**Fields**:
| Field | Type | Purpose |
|-------|------|---------|
| `next` | `*mut HandleSlot` | Next free slot |
| `limit` | `*mut HandleSlot` | End of current block |
| `level` | `u32` | Nesting depth |
| `sealed_level` (debug) | `u32` | Level at/below which allocation is forbidden |

**Invariant**: `next <= limit` always true

**Debug Invariant**: Allocation panics if `level <= sealed_level`

---

## Validation Rules

### HandleScope Lifecycle

1. **Creation**: `HandleScope::new(tcb)` must receive valid `ThreadControlBlock`
2. **Allocation**: `allocate_slot()` must have `next < limit`
3. **Drop**: Must restore `next`, `limit`, `level` to saved values

### Handle Validity

1. **Creation**: Handle slot must be initialized with valid `GcBox` pointer
2. **Access**: `Handle::get()` reads from valid slot
3. **Lifetime**: `Handle<'scope, T>` must not outlive `HandleScope`

### Escape Constraints

1. Single escape per `EscapeableHandleScope`
2. Escape slot pre-allocated in parent scope
3. Returned handle bound to parent scope lifetime

---

## State Diagrams

### HandleScope State Machine

```
                    +------------------+
                    |                  |
                    v                  |
    +--------> INACTIVE                |
    |           |    ^                 |
    |           |    |                 |
    |           v    |                 |
    |     +-----------+---+             |
    |     |               |             |
    |     |    ACTIVE     |             |
    |     |               |             |
    |     +---------------+             |
    |                 |                 |
    +-----------------+-----------------+
                  (drop)
```

### EscapeableHandleScope State Machine

```
    +----------+    new()     +----------+    escape()    +----------+
    |          | ---------->  |          |  ----------->  |          |
    |  FRESH   |              |  READY   |                | ESCAPED  |
    |          |              |          |                |          |
    +----------+              +----------+                +----------+
                                      |
                                      | escape()
                                      v
                                   panic!
```

---

## Relationships Diagram

```
ThreadControlBlock
       |
       +-- local_handles: LocalHandles
       |        |
       |        +-- blocks: HandleBlock*
       |        |        |
       |        |        +-- slots[256]: HandleSlot
       |        |        |
       |        |        +-- next: HandleBlock*
       |        |
       |        +-- scope_data: HandleScopeData
       |                 |
       |                 +-- next: HandleSlot*
       |                 +-- limit: HandleSlot*
       |                 +-- level: u32
       |
       +-- async_scopes: Vec<AsyncHandleScope*>
                |
                +-- tcb: Arc<ThreadControlBlock>
                +-- block: HandleBlock
                +-- used: AtomicUsize
                +-- dropped: AtomicBool

HandleScope<'env>
       |
       +-- handles: &'env mut LocalHandles
       +-- prev_next: HandleSlot*
       +-- prev_limit: HandleSlot*
       +-- prev_level: u32

Handle<'scope, T>
       |
       +-- slot: HandleSlot*
       +-- _marker: PhantomData<(&'scope (), *const T)>

AsyncHandle<T>
       |
       +-- slot: HandleSlot*
       +-- _marker: PhantomData<*const T)>
```
