# Feature Specification: HandleScope v2 Implementation

**Feature Branch**: `006-handlescope-v2-impl`
**Created**: 2026-02-01
**Status**: Draft
**Input**: User description: "implement handlescope! ignore Interior pointer fix(already fixed), v8: @learn-projects/v8/ rudo-gc: @crates/rudo-gc/"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Create and Use Handles in Scope (Priority: P1)

As a Rust developer using rudo-gc, I want to create handles within a defined scope so that I can safely access garbage-collected objects with compile-time lifetime guarantees.

**Why this priority**: This is the core functionality that enables all other HandleScope features. Without this, developers cannot use the new v2 API.

**Independent Test**: Can be fully tested by creating a HandleScope, allocating Gc objects, creating handles from them, and verifying handles remain valid within scope but become invalid after scope ends.

**Acceptance Scenarios**:

1. **Given** a valid ThreadControlBlock, **When** I create a HandleScope, **Then** I can allocate Gc objects and create handles via `scope.handle(&gc)`, **And** handles remain accessible while scope is active.

2. **Given** a handle created within an active HandleScope, **When** the scope ends (goes out of scope), **Then** attempting to use the handle results in a compile-time error (lifetime constraint violation).

3. **Given** nested HandleScopes, **When** I create handles in inner scopes, **Then** those handles are only valid within their creation scope and cannot escape to outer scopes (enforced by type system).

---

### User Story 2 - Escape Handles to Parent Scope (Priority: P2)

As a Rust developer, I need to create a handle in an inner scope and return it to the outer scope so that I can build data structures that require initialization logic.

**Why this priority**: Common pattern when factory functions need to initialize objects before returning them. The EscapeableHandleScope enables this pattern safely.

**Independent Test**: Can be fully tested by creating an EscapeableHandleScope, creating a handle inside it, calling escape() to return it to the parent scope, and verifying the escaped handle remains valid in the outer scope.

**Acceptance Scenarios**:

1. **Given** an EscapeableHandleScope, **When** I create a handle and call `escape()` once, **Then** the returned handle is valid in the parent scope.

2. **Given** an EscapeableHandleScope where escape() was already called, **When** I call escape() again, **Then** the operation panics (prevents multiple escapes).

3. **Given** a handle escaped via EscapeableHandleScope, **When** the inner scope ends, **Then** the escaped handle remains valid and accessible in the outer scope.

---

### User Story 3 - Prevent Handle Creation in Critical Sections (Priority: P3)

As a library author or performance profiler, I want to prevent accidental handle allocations in critical code sections so that I can ensure predictable performance and detect memory issues early.

**Why this priority**: Debug-only feature for catching bugs and ensuring performance predictability. Important for production debugging scenarios.

**Independent Test**: Can be fully tested by creating a SealedHandleScope in debug mode and verifying that handle creation attempts trigger panics.

**Acceptance Scenarios**:

1. **Given** debug build with SealedHandleScope active, **When** I attempt to create a handle, **Then** the operation panics with a clear error message.

2. **Given** release build (debug assertions disabled), **When** I create a SealedHandleScope, **Then** it becomes a no-op and handle creation proceeds normally (zero overhead in release).

---

### User Story 4 - Use GC Handles in Async/Await Code (Priority: P1)

As a Rust developer writing async code with tokio, I want to access garbage-collected objects across await points so that I can use rudo-gc in modern async Rust applications.

**Why this priority**: Async/await is the standard way to write concurrent Rust code. Without proper async support, rudo-gc cannot be used in real-world async applications.

**Independent Test**: Can be fully tested by creating an AsyncHandleScope, allocating Gc objects, creating async handles, awaiting async operations, and verifying handles remain valid across await points.

**Acceptance Scenarios**:

1. **Given** an AsyncHandleScope within an async function, **When** I create handles and await async operations, **Then** all handles remain valid and accessible after each await.

2. **Given** multiple concurrent async tasks using AsyncHandleScope, **When** garbage collection runs, **Then** all active handles are correctly identified as GC roots.

3. **Given** an AsyncHandleScope that is dropped, **When** subsequent code attempts to use the async handles, **Then** the handles become invalid and any access triggers undefined behavior (programmer error, documented via unsafe contract).

---

### User Story 5 - Safely Spawn Async Tasks with GC Roots (Priority: P1)

As a Rust developer, I want to spawn async tasks that safely track GC roots so that I can use tokio::spawn with rudo-gc objects without memory safety issues.

**Why this priority**: `tokio::spawn` is the primary way to run async work. The current v1 approach with `root_guard()` is error-prone (easy to forget). The macro provides a safe, ergonomic alternative.

**Independent Test**: Can be fully tested using spawn_with_gc! macro with single and multiple Gc objects, verifying handles remain valid throughout the async task lifetime.

**Acceptance Scenarios**:

1. **Given** a Gc object, **When** I use `spawn_with_gc!(gc => |handle| async move { ... })`, **Then** the handle remains valid for the entire async task execution.

2. **Given** multiple Gc objects, **When** I use `spawn_with_gc!(gc1, gc2 => |h1, h2| async move { ... })`, **Then** both handles are accessible within the async block.

3. **Given** a spawned task with GC roots, **When** garbage collection runs during task execution, **Then** all tracked Gc objects survive.

---

### Edge Cases

- What happens when handle block capacity is exhausted? **System allocates new handle blocks automatically, with reasonable limits to prevent runaway allocation.**
- How does system handle nested HandleScopes with different escape requirements? **Each scope manages its own handle allocation state; escape operations must be planned at the outer scope level.**
- How does GC interact with handles in suspended async tasks? **AsyncHandleScope maintains strong references; all handles are visited during GC marking phase.**
- What happens if ThreadControlBlock is dropped while handles are active? **Undefined behavior - callers must ensure all handles are dropped before TCB lifecycle ends (documented in API contracts).**

---

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide HandleScope type that defines a lexical scope for handle validity, where all handles created within the scope are automatically invalidated when the scope ends.
- **FR-002**: System MUST ensure Handle<'scope, T> lifetime parameter is bound to the HandleScope that created it, preventing handles from escaping their scope without explicit escape mechanism.
- **FR-003**: System MUST provide EscapeableHandleScope that allows exactly one handle to escape to the parent scope via explicit escape() operation.
- **FR-004**: System MUST provide SealedHandleScope that prevents new handle creation within its scope, available only in debug builds with zero overhead in release.
- **FR-005**: System MUST provide AsyncHandleScope for async/await contexts where handles must survive across await points and task suspension.
- **FR-006**: System MUST provide AsyncHandle<T> type for async contexts, where handle validity is managed by the parent AsyncHandleScope rather than lifetime parameters.
- **FR-007**: System MUST provide spawn_with_gc! macro that automatically creates AsyncHandleScope and tracks GC roots for spawned async tasks.
- **FR-008**: System MUST integrate handle root tracking with GC marking phase, ensuring all handles are visited and their referenced Gc objects remain alive.
- **FR-009**: System MUST extend ThreadControlBlock with local handle management state and async scope registration for GC integration.
- **FR-010**: System MUST provide LocalHandles, HandleBlock, HandleSlot, HandleScopeData internal data structures for efficient handle storage and iteration.

### Key Entities

- **HandleScope<'env>**: Defines lexical scope boundary for handle validity. Contains previous scope state for restoration on drop. Created with ThreadControlBlock reference.
- **Handle<'scope, T>**: Lifetime-bound reference to Gc<T>. Dereferences to &T. Not Send/Sync (thread-local by design). Copyable.
- **EscapeableHandleScope<'env>**: HandleScope variant allowing one handle to escape. Contains escape slot pre-allocated in parent scope. Single-use escape().
- **SealedHandleScope<'env>**: Debug-only scope that prevents handle creation. Zero-cost in release builds.
- **AsyncHandleScope**: Async-safe scope using Arc<ThreadControlBlock>. Manages dedicated handle block with atomic slot counter.
- **AsyncHandle<T>**: Async context handle without lifetime parameter. Unsafe to use after parent scope is dropped (documented contract).
- **LocalHandles**: Per-thread handle storage manager. Manages linked list of HandleBlocks. Provides scope_data for HandleScope operations.
- **HandleBlock**: Fixed-size array of HandleSlots (typically 256). Allocated dynamically as needed.
- **HandleSlot**: Individual handle storage containing pointer to GcBox.
- **HandleScopeData**: Runtime state for HandleScope (next, limit, level). Stored in LocalHandles.

---

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Developers can complete HandleScope v2 migration from v1 with zero runtime overhead in release builds (handle allocation is O(1) bump allocation).
- **SC-002**: All handles are correctly tracked as GC roots, eliminating false positives and false negatives in garbage collection.
- **SC-003**: AsyncHandleScope enables safe GC root tracking across arbitrary await points, with handles remaining valid throughout async task lifetime.
- **SC-004**: The spawn_with_gc! macro eliminates the need for manual root_guard() calls, reducing programmer error in async GC usage.
- **SC-005**: Compile-time lifetime checking prevents handle escape without explicit EscapeableHandleScope, catching bugs at compile time rather than runtime.
- **SC-006**: Interior pointer support (already implemented) correctly identifies GcBox from any field pointer, fixing UAF vulnerabilities.

---

## Assumptions

- Interior pointer fix is already implemented in find_gc_box_from_ptr (as stated in user description).
- ThreadControlBlock is already available and accessible where HandleScope is needed.
- The Trace trait and Gc<T> types are already implemented and functional.
- Tok integration follows existing patterns from tokio/ module.
- Handle block size of 256 slots is appropriate default (can be made configurable if needed).
