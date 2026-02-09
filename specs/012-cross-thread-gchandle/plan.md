# Implementation Plan: Cross-Thread GC Handle System

**Branch**: `012-cross-thread-gchandle` | **Date**: 2026-02-10 | **Spec**: [link](../spec.md)
**Input**: Feature specification from `/specs/012-cross-thread-gchandle/spec.md`, Approved Implementation Plan from `@docs/012-cross-thread-handle-plan.md`

## Summary

Implement a cross-thread handle system for rudo-gc that allows safe hand-off of GC-managed object references between threads. The feature introduces `GcHandle<T>` and `WeakCrossThreadHandle<T>` types that are `Send + Sync` regardless of whether `T` implements those traits. Resolution back to `Gc<T>` is only permitted on the origin thread, enforced at runtime via thread ID checks. Strong handles keep referenced objects alive through root registration on the origin thread's `ThreadControlBlock`. This enables frameworks like Rvue to schedule UI updates from async threads without requiring signal types themselves to be `Send + Sync`.

## Technical Context

**Language/Version**: Rust 1.75+ (stable) | **Target Crate**: `rudo-gc`  
**Primary Dependencies**: None (uses existing `ThreadId`, `ThreadControlBlock`, `Arc`, `Mutex`, `HashMap`)  
**Storage**: N/A (in-memory GC heap)  
**Testing**: cargo test with Miri verification for unsafe code  
**Target Platform**: Cross-platform (x86_64/aarch64, Linux/macOS/Windows)  
**Project Type**: Rust library crate (garbage collector)  
**Performance Goals**: Lock-free resolve/try_resolve hot path, O(1) handle operations, minimal contention on TCB mutex  
**Constraints**: Must maintain compatibility with existing GC features (008 incremental marking, 009 tracing, 011 concurrent GC primitives); must not introduce deadlocks; ThreadId comparison must be fast  
**Scale/Scope**: Supports arbitrary number of cross-thread handles; HashMap provides O(1) insert/remove per handle

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

### Memory Safety (NON-NEGOTIABLE)

| Requirement | Status | Implementation Approach |
|-------------|--------|------------------------|
| Unsafe code MUST have explicit SAFETY comments | ✅ Required | All unsafe blocks in GcHandle/WeakCrossThreadHandle will include comprehensive SAFETY comments |
| GC MUST never access freed memory | ✅ Required | Handle validity guaranteed by root registration; Gc::from_raw only called after ThreadId check passes |
| Marker-based type system MUST correctly convey ownership | ✅ Required | PhantomData usage in handle types to maintain Send/Sync semantics |

### Testing Discipline (NON-NEGOTIABLE)

| Requirement | Status | Implementation Approach |
|-------------|--------|------------------------|
| All new features MUST have corresponding tests | ✅ Required | 13 integration tests covering all operations |
| Unsafe code MUST pass Miri tests | ✅ Required | test_miri_thread_safety included |
| GC interference tests MUST use --test-threads=1 | ✅ Required | Follows project convention |

### Performance-First Design

| Requirement | Status | Implementation Approach |
|-------------|--------|------------------------|
| Allocation O(1) via BiBOP | ✅ Compatible | Feature doesn't affect allocation |
| Collection pauses minimized | ✅ Compatible | Lock-free resolve hot path; cross_thread_roots lock held briefly |
| Memory overhead predictable | ✅ Compatible | Handle storage is O(1) per handle |

### API Consistency

| Requirement | Status | Implementation Approach |
|-------------|--------|------------------------|
| snake_case for functions/methods | ✅ Followed | cross_thread_handle, weak_cross_thread_handle, try_resolve, unregister |
| PascalCase for types | ✅ Followed | GcHandle, WeakCrossThreadHandle, HandleId |
| Result<T,E> for recoverable errors | ✅ N/A | Uses panic for unrecoverable (wrong-thread access), Option for try_resolve |
| Doc comments with examples | ✅ Required | API documentation comments with usage examples |

### Cross-Platform Reliability

| Requirement | Status | Implementation Approach |
|-------------|--------|------------------------|
| Consistent across x86_64/aarch64 | ✅ Required | Uses std::thread::current().id() which is platform-consistent |
| Tests pass identically on all platforms | ✅ Required | No platform-specific code in handles |

## Project Structure

### Documentation (this feature)

```text
specs/012-cross-thread-gchandle/
├── plan.md              # This file (/speckit.plan command output)
├── spec.md              # Feature specification (/speckit.specify command output)
├── research.md          # N/A - implementation plan already approved
├── data-model.md        # N/A - data model defined in implementation plan
├── quickstart.md        # Usage guide with examples
├── contracts/           # N/A - internal Rust API, not external contracts
└── tasks.md             # Phase 2 output (/speckit.tasks command)
```

### Source Code (repository root)

```text
crates/rudo-gc/src/
├── handles/
│   ├── mod.rs               # Export cross_thread module
│   └── cross_thread.rs      # GcHandle, WeakCrossThreadHandle, HandleId (NEW)
├── heap.rs                   # ThreadControlBlock additions
├── gc/
│   └── gc.rs                 # mark_cross_thread_roots() integration
├── ptr.rs                    # Gc::cross_thread_handle() methods
└── lib.rs                    # Exports

tests/
└── cross_thread_handle.rs    # Integration tests (NEW)
```

**Structure Decision**: New `handles/` module added to `crates/rudo-gc/src/` containing cross-thread handle types. GC integration in existing `gc/gc.rs`. ThreadControlBlock extensions in `heap.rs`. Extension methods on `Gc<T>` in `ptr.rs`.

## Technical Design

### Primary Types

**GcHandle<T: Trace + 'static>**

Strong cross-thread handle that keeps the referenced object alive. Fields:
- `ptr: NonNull<GcBox<T>>` - Raw pointer to the GcBox, validity guaranteed by root registration
- `origin_tcb: Arc<ThreadControlBlock>` - TCB of origin thread, prevents TCB deallocation
- `origin_thread: ThreadId` - Origin thread identity for resolve-time check
- `handle_id: HandleId` - Unique ID for O(1) unregistration

**WeakCrossThreadHandle<T: Trace + 'static>**

Weak cross-thread handle that does not prevent collection. Fields:
- `weak: GcBoxWeakRef<T>` - Weak reference to the GcBox
- `origin_tcb: Arc<ThreadControlBlock>` - TCB of origin thread
- `origin_thread: ThreadId` - Origin thread identity

**HandleId(u64)**

Opaque ID for registered cross-thread handle root entry. Includes INVALID sentinel (u64::MAX) for unregistered state.

### Safety Invariants

1. **No direct access to T from non-origin threads** - Handle is opaque token storing no reference through which T can be read/written. Only resolve() produces Gc<T>, enforced at runtime.

2. **Origin-thread enforcement is hard check** - resolve() compares std::thread::current().id() against stored origin_thread. This is panic, not UB.

3. **Root registration keeps object alive** - Handle holds Arc<ThreadControlBlock>. Root entry in TCB's Mutex-protected handle list. GC always finds and marks referenced object. Root outlives origin thread's stack.

4. **WeakCrossThreadHandle also enforces origin-thread affinity** - Resolving produces Gc<T>, which for T: !Send must not exist on foreign thread.

5. **'static bound required** - Handles may outlive scope where object was allocated. Prevents dangling lifetime references.

### GC Integration

**Root Storage on ThreadControlBlock**

Handles stored on origin thread's ThreadControlBlock, not thread-local LocalRoots. This enables safe cross-thread Drop.

```rust
struct ThreadControlBlock {
    cross_thread_roots: Mutex<CrossThreadRootTable>,
}

struct CrossThreadRootTable {
    next_id: u64,
    strong: HashMap<HandleId, NonNull<GcBox<()>>>,
}
```

**Why ThreadControlBlock over LocalRoots**

LocalRoots accessed through thread-local storage, not safely accessible from other threads. ThreadControlBlock is Arc-shared and already used for cross-thread coordination. Mutex has minimal contention.

**GC Root Marking**

During mark phase, collector iterates cross-thread handle root entries:

```rust
fn mark_cross_thread_roots(tcb: &ThreadControlBlock, visitor: &mut GcVisitor) {
    let roots = tcb.cross_thread_roots.lock();
    for (_id, ptr) in &roots.strong {
        unsafe { visitor.mark(*ptr); }
    }
}
```

Uses mark() (adds to worklist), not mark_gray(). Cross-thread handle roots are strong roots semantically identical to stack roots.

### Handle Creation (Atomicity Guarantee)

Handle creation must be atomic with respect to GC. Object must not be collected between obtaining pointer and registering root. Achieved by holding TCB's root table lock across both operations:

```rust
pub fn cross_thread_handle(&self) -> GcHandle<T> {
    let tcb = current_thread_tcb();
    let mut roots = tcb.cross_thread_roots.lock();
    let handle_id = roots.allocate_id();
    let ptr = self.as_non_null();
    roots.strong.insert(handle_id, ptr.cast::<GcBox<()>>());
    drop(roots);
    GcHandle { ptr, origin_tcb: Arc::clone(&tcb), origin_thread: current_thread_id(), handle_id }
}
```

### Drop from Any Thread

```rust
impl Drop for GcHandle<T> {
    fn drop(&mut self) {
        let mut roots = self.origin_tcb.cross_thread_roots.lock();
        roots.strong.remove(&self.handle_id);
    }
}
```

No thread-local storage access. Arc<ThreadControlBlock> keeps TCB alive even if origin thread exits.

### Resolve Implementation

```rust
pub fn resolve(&self) -> Gc<T> {
    assert_eq!(std::thread::current().id(), self.origin_thread,
        "GcHandle::resolve() must be called on the origin thread");
    unsafe { Gc::from_raw(self.ptr) }
}

pub fn try_resolve(&self) -> Option<Gc<T>> {
    if std::thread::current().id() != self.origin_thread { return None; }
    Some(unsafe { Gc::from_raw(self.ptr) })
}
```

ThreadId comparison is the hot path - lock-free, no allocation.

### Thread Exit Behavior

When origin thread exits while handles exist:
- TCB not deallocated (Arc reference)
- Root entries remain valid
- resolve() panics (ThreadId check fails)
- Object remains alive (roots prevent collection)
- is_valid() remains correct (checks registration state)
- Drop remains safe (mutex access only)

### Unregister Semantics

```rust
pub fn unregister(&mut self) {
    let mut roots = self.origin_tcb.cross_thread_roots.lock();
    roots.strong.remove(&self.handle_id);
    self.handle_id = HandleId::INVALID;
}
```

Idempotent. After unregistration: resolve() panics, Drop is no-op, is_valid() returns false.

### Clone Implementation

```rust
fn clone(&self) -> Self {
    let mut roots = self.origin_tcb.cross_thread_roots.lock();
    let new_id = roots.allocate_id();
    roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
    GcHandle { ptr: self.ptr, origin_tcb: Arc::clone(&self.origin_tcb),
               origin_thread: self.origin_thread, handle_id: new_id }
}
```

Each clone independently registers a root. Dropping one doesn't affect liveness of others.

### Interaction with Incremental Marking (Feature 008)

When cross-thread handle resolved during active incremental marking phase, resulting Gc<T> stored into already-marked object handled by existing write barrier in GcCell::borrow_mut():
- SATB barrier captures old pointer value before mutation
- Dijkstra insertion barrier marks new pointer value immediately

No additional barriers needed in resolve() - only produces local Gc<T>, doesn't perform store.

### Thread Safety Implementations

```rust
unsafe impl<T: Trace + 'static> Send for GcHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for GcHandle<T> {}
unsafe impl<T: Trace + 'static> Send for WeakCrossThreadHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for WeakCrossThreadHandle<T> {}
```

SAFETY: GcHandle never exposes T directly. Only path to T is resolve(), which enforces origin-thread affinity. Handle's internal state (ptr, Arc<TCB>, ThreadId, HandleId) is all Send+Sync-safe.

## Design Decisions

| Decision | Rationale |
|----------|-----------|
| `gc.cross_thread_handle()` method name | Explicit naming for cross-thread primitives |
| Roots stored on ThreadControlBlock (not LocalRoots) | Critical: enables safe Drop from any thread. TCB is Arc-shared; LocalRoots requires thread-local storage |
| Arc<ThreadControlBlock> in handle | Prevents TCB deallocation after origin thread exits. Enables foreign-thread Drop |
| HandleId + HashMap for root entries | O(1) insert/remove. Vec would be O(n) removal on drop |
| Raw NonNull<GcBox<T>> (not weak ref) for strong handle | Root registration guarantees liveness. Weak ref adds indirection with no benefit |
| Panic on wrong thread for resolve() | Fail-fast for incorrect usage; matches Rust idioms |
| try_resolve() variant | Graceful alternative for contexts where thread identity is uncertain |
| WeakCrossThreadHandle also enforces origin-thread affinity | Safety: resolving produces Gc<T> which must not exist on foreign thread when T: !Send |
| Strong handle semantics by default | Matches Rvue's primary use case |
| Weak handle variant included | Future-proofing for "schedule if alive" patterns |
| Handle unregistration is idempotent | Avoids double-free footgun; Drop after unregister is no-op |
| Clone registers independent root | Each clone is first-class root; no reference counting on root entries |
| T: 'static bound required | Handle may outlive creating scope; prevents dangling lifetime refs in T |
| Thread exit → resolve panics, object stays alive | Simpler than handle migration. Object floats until handles drop. No UB |
| No barrier in resolve() | Barriers belong at stores (GcCell::borrow_mut), not at reads. Existing barriers handle correctly |

## Lock Ordering

Extended existing lock ordering discipline:

```
LocalHeap → GlobalMarkState → GcRequest → CrossThreadRootTable
```

CrossThreadRootTable (per-TCB mutex) acquired last. During GC root scanning, collector already holds higher-level locks before iterating TCBs.

Handle creation and drop only acquire cross_thread_roots - no risk of inversion.

## Performance Considerations

| Operation | Cost | Notes |
|-----------|------|-------|
| cross_thread_handle() | Mutex lock + HashMap insert | Cold path; acceptable |
| resolve() | ThreadId comparison + pointer deref | Hot path; no lock, no allocation |
| try_resolve() | ThreadId comparison + pointer deref | Hot path; no lock, no allocation |
| clone() | Mutex lock + HashMap insert + Arc clone | Cold path; acceptable |
| drop() | Mutex lock + HashMap remove | Cold path; acceptable |
| GC root scan | Mutex lock + HashMap iteration | Once per GC cycle per thread; O(n) in handle count |

Hot path (resolve/try_resolve) is lock-free. All lock-taking operations on cold paths.

## Dependencies & Compatibility

- **No new external dependencies** - uses existing ThreadId, ThreadControlBlock, Arc, Mutex, HashMap
- **Compatible with existing features**: 008 (incremental marking), 009 (tracing), 011 (concurrent GC primitives)
- **Feature flag**: No new feature flag needed; part of core API

## Implementation Steps

### Step 1: Core Infrastructure (heap.rs)

- [ ] Add CrossThreadRootTable struct with HashMap<HandleId, NonNull<GcBox<()>>>
- [ ] Add HandleId type with INVALID sentinel
- [ ] Add cross_thread_roots: Mutex<CrossThreadRootTable> to ThreadControlBlock
- [ ] Implement allocate_id() on CrossThreadRootTable

### Step 2: Core Types (handles/cross_thread.rs)

- [ ] Define GcHandle<T> struct (ptr, origin_tcb, origin_thread, handle_id)
- [ ] Define WeakCrossThreadHandle<T> struct (weak, origin_tcb, origin_thread)
- [ ] Implement Send + Sync with // SAFETY comments for both
- [ ] Implement Drop for GcHandle (mutex-based, any-thread safe)
- [ ] Implement Clone for both types
- [ ] Implement Debug for both types
- [ ] Implement origin_thread(), is_valid(), unregister() on GcHandle
- [ ] Implement resolve() and try_resolve() on both types

### Step 3: Gc Extension Methods (ptr.rs)

- [ ] Add Gc::cross_thread_handle() - atomic registration under lock
- [ ] Add Gc::weak_cross_thread_handle()
- [ ] Add GcHandle::downgrade() method

### Step 4: GC Integration (gc/gc.rs)

- [ ] Add mark_cross_thread_roots() function
- [ ] Call it from mark_all_roots() during root scanning phase
- [ ] Ensure lock ordering is documented: cross_thread_roots lock acquired after LocalHeap lock

### Step 5: Module Exports (handles/mod.rs, lib.rs)

- [ ] Export GcHandle and WeakCrossThreadHandle types
- [ ] Add to public API documentation

### Step 6: Tests

- [ ] test_cross_thread_send: Handle sent between threads via channel
- [ ] test_resolve_origin_thread: Verify resolve() panics on wrong thread
- [ ] test_try_resolve_wrong_thread: Verify try_resolve() returns None
- [ ] test_handle_keeps_alive: Verify object not collected while handle exists
- [ ] test_weak_handle_no_prevent: Verify weak handle doesn't prevent collection
- [ ] test_is_valid_checks: Verify is_valid() reflects registration state
- [ ] test_clone_independent_lifetime: Cloned handles are independent roots
- [ ] test_unregister_idempotent: Double unregister is safe
- [ ] test_drop_from_foreign_thread: Drop on non-origin thread is safe
- [ ] test_multiple_handles_same_object: Multiple handles to same object
- [ ] test_origin_thread_exit: Behavior when origin thread exits
- [ ] test_downgrade: Strong-to-weak downgrade
- [ ] test_miri_thread_safety: Miri verification for unsafe code

### Step 7: Documentation

- [ ] Update AGENTS.md with new feature
- [ ] Add API documentation comments
- [ ] Example usage in doc tests

## Testing Strategy

| Test | Description |
|------|-------------|
| test_cross_thread_send | Handle sent between threads via channel |
| test_resolve_origin_thread | Verify resolve() panics on wrong thread |
| test_try_resolve_wrong_thread | Verify try_resolve() returns None on wrong thread |
| test_handle_keeps_alive | Verify object not collected while handle exists |
| test_weak_handle_no_prevent | Verify weak handle doesn't prevent collection |
| test_is_valid_checks | Verify is_valid() accuracy |
| test_clone_independent_lifetime | Clone keeps object alive independently |
| test_unregister_idempotent | Double unregister doesn't panic |
| test_drop_from_foreign_thread | Handle dropped on non-origin thread |
| test_multiple_handles | Multiple handles to same object |
| test_origin_thread_exit | Behavior when origin thread exits |
| test_downgrade | Strong-to-weak downgrade preserves semantics |
| test_miri_thread_safety | Miri verification for unsafe code |

All tests use --test-threads=1 to avoid GC interference between parallel test threads.

## Usage Examples

### Rvue Pattern

```rust
// UI Thread — create handle
let signal_gc: Gc<SignalDataInner<T>> = create_signal();
let handle: GcHandle<SignalDataInner<T>> = signal_gc.cross_thread_handle();

// Send handle to async thread (handle is Send + Sync, T need not be)
tokio::spawn(async move {
    let result = async_work().await;
    channel.send((handle, result));
});

// UI Thread — resolve and update
for (handle, value) in receiver {
    let signal: Gc<SignalDataInner<T>> = handle.resolve();
    signal.set(value);
    // handle is dropped here, root entry is removed
}
```

### Defensive Pattern (Unknown Thread)

```rust
// When you're not sure which thread you're on:
if let Some(signal) = handle.try_resolve() {
    signal.set(value);
} else {
    // Not on origin thread, or handle was unregistered.
    // Queue the update for the origin thread instead.
    origin_sender.send(UpdateMsg { handle: handle.clone(), value });
}
```

## Deferred Items (Future Features)

1. Handle migration: Allow migrating handles to different threads (thread pools)
2. Handle registry: Global registry for handles that survive thread exit with named lookup
3. AsyncHandle integration: Allow GcHandle to be used with AsyncHandleScope
4. Batch resolution: resolve_many(&[GcHandle<T>]) -> Vec<Gc<T>> for bulk operations

## References

- Feature Request: `rudo-gc-feature-request-cross-thread-handle.md`
- Existing Infrastructure: AsyncHandleScope, GcRootSet, LocalHeap, ThreadControlBlock
- Thread Model: ThreadControlBlock, ThreadRegistry
- Lock Ordering: See heap.rs documentation and Feature 001

## Code Quality Gates

All pull requests MUST satisfy:

1. **Lint**: `./clippy.sh` passes with zero warnings
2. **Format**: `cargo fmt --all` produces no changes
3. **Test**: `./test.sh` passes all tests (including ignored)
4. **Safety**: `./miri-test.sh` passes for unsafe code changes
5. **Documentation**: Public APIs have doc comments with examples

## Complexity Tracking

> Fill ONLY if Constitution Check has violations that must be justified

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| N/A | This feature follows all constitution requirements | N/A |

## Quick Reference

**Branch**: `012-cross-thread-gchandle`  
**Implementation Plan**: `/home/noah/Desktop/rudo/specs/012-cross-thread-gchandle/plan.md`  
**Next Phase**: `/speckit.tasks` to generate implementation tasks
