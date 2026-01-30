# Feature Specification: Tokio Async/Await Integration

**Feature Branch**: `[004-tokio-async-integration]`
**Created**: 2026-01-30
**Status**: Draft
**Input**: User description: "support tokio async/await @docs/tokio-async-integration-plan-v3.md "

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Basic Async Gc Usage with Manual Root Guards (Priority: P1)

As a Rust developer using rudo-gc in a tokio async application, I want to use Gc pointers inside async tasks with proper root tracking so that my garbage-collected objects remain valid during async execution.

**Why this priority**: This is the foundational capability that enables all other tokio integration features. Without reliable root tracking in async contexts, developers cannot safely use rudo-gc with tokio at all.

**Independent Test**: Can be fully tested by creating a Gc pointer, manually creating a root guard, spawning an async task that accesses the Gc, and verifying the Gc remains valid throughout task execution without premature collection.

**Acceptance Scenarios**:

1. **Given** a Gc pointer created in an async context, **When** a root guard is manually created for that pointer, **Then** the Gc must remain valid for the duration of the guard's lifetime.

2. **Given** a root guard goes out of scope, **When** the GC runs, **Then** the previously guarded Gc object becomes eligible for collection if no other references exist.

3. **Given** multiple Gc pointers, **When** root guards are created for each, **Then** all must remain valid until their respective guards are dropped.

4. **Given** a tokio task spawned with the Gc moved into it, **When** the task executes, **Then** the Gc must be accessible within the task if a root guard was created.

---

### User Story 2 - Proc-Macro Automation with #[gc::main] and #[gc::root] (Priority: P2)

As a Rust developer, I want to use procedural macros to automatically manage GC roots in my async code so that I don't have to manually create and drop guards.

**Why this priority**: Manual root management is error-prone and verbose. Proc-macro automation provides a better developer experience and reduces the chance of memory safety issues from forgotten root guards.

**Independent Test**: Can be fully tested by annotating an async function with #[gc::main], creating Gc objects inside, and spawning tasks with #[gc::root], then verifying all Gc objects are properly tracked without explicit guard code.

**Acceptance Scenarios**:

1. **Given** an async function annotated with #[gc::main], **When** the function is called, **Then** a tokio runtime must be created and the GcRootSet must be initialized.

2. **Given** an async block annotated with #[gc::root], **When** the block executes, **Then** all Gc pointers accessed within must be automatically protected by a root guard.

3. **Given** nested async functions with #[gc::root], **When** they execute concurrently, **Then** each must have independent root tracking that doesn't interfere with others.

---

### User Story 3 - Automatic Root Tracking with gc::spawn Wrapper (Priority: P2)

As a Rust developer, I want spawned async tasks to automatically track Gc roots so that I don't have to manually manage guards for every spawned task.

**Why this priority**: Spawning tasks is a common pattern in tokio applications. Automatic root tracking via gc::spawn significantly reduces boilerplate and improves safety.

**Independent Test**: Can be fully tested by using gc::spawn to run async tasks that access Gc pointers, without creating explicit root guards, and verifying the Gc remains valid throughout task execution.

**Acceptance Scenarios**:

1. **Given** gc::spawn is called with an async closure that captures a Gc pointer, **When** the task executes, **Then** the Gc must be protected for the task's lifetime.

2. **Given** multiple tasks spawned via gc::spawn, **When** each accesses different Gc pointers, **Then** all must be independently protected.

3. **Given** a task spawned via gc::spawn completes, **When** its guard goes out of scope, **Then** any associated roots must be properly unregistered.

---

### User Story 4 - Cooperative GC Scheduling with yield_now (Priority: P3)

As a Rust developer building long-running async computations with Gc, I want to periodically yield to the tokio scheduler so that the GC can run without blocking my computation.

**Why this priority**: Long-running async tasks need to cooperate with the GC to prevent memory buildup. Providing a yield_now method allows controlled GC intervention.

**Independent Test**: Can be fully tested by creating a long-running loop that calls yield_now periodically, verifying that GC cycles can occur during yields without task starvation.

**Acceptance Scenarios**:

1. **Given** an async task calls yield_now on a Gc pointer, **When** the yield completes, **Then** the task resumes and can continue accessing the Gc.

2. **Given** a loop that repeatedly calls yield_now, **When** GC runs between yields, **Then** memory should be reclaimed from unreachable objects.

3. **Given** a task that never calls yield_now, **When** memory pressure builds, **Then** the system must still be able to trigger GC collection.

---

### User Story 5 - Multi-Runtime Support (Priority: P3)

As a Rust developer running multiple tokio runtimes, I want all runtimes to share a single GcRootSet so that Gc objects can be accessed across runtime boundaries.

**Why this priority**: Complex applications may use multiple tokio runtimes for different subsystems. Process-level root tracking enables this use case.

**Independent Test**: Can be fully tested by creating multiple tokio runtimes, creating Gc objects in each, and verifying all are tracked correctly regardless of which runtime they were created on.

**Acceptance Scenarios**:

1. **Given** two tokio runtimes, **When** Gc objects are created and accessed in tasks on each, **Then** all must be tracked in a single process-level root set.

2. **Given** a Gc created on runtime A, **When** accessed from a task on runtime B, **Then** the Gc must remain valid if a root guard exists.

3. **Given** tasks on different runtimes that share Gc references, **When** GC runs, **Then** all roots from both runtimes must be scanned.

---

### User Story 6 - GC Notification via Dirty Flag (Priority: P3)

As a Rust developer, I want the GC to be notified when roots are added or removed so that collection happens at appropriate times.

**Why this priority**: Efficient GC requires knowing when roots change. The dirty flag enables the GC to skip unnecessary collection cycles.

**Independent Test**: Can be fully tested by registering and unregistering roots, then verifying the dirty flag is set and cleared appropriately.

**Acceptance Scenarios**:

1. **Given** GcRootSet is initially clean, **When** a root is registered, **Then** the dirty flag must be set to true.

2. **Given** GcRootSet is dirty, **When** GC takes a snapshot and clears the flag, **Then** the dirty flag must be false until the next root change.

3. **Given** roots are modified rapidly, **When** GC runs, **Then** it must capture the most recent root state.

---

### Edge Cases

- What happens when nested spawns create multiple layers of root guards?
- How does the system handle a task that panics while holding a root guard?
- What occurs when Gc pointers are cloned while a root guard is active?
- How are dropped Gc objects handled when roots are still registered?
- What happens if gc::spawn is called without the tokio feature enabled?
- How does the system behave when yield_now is called from a non-tokio context?
- What are the semantics of moving Gc pointers between async tasks with different lifetimes?

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide a process-level GcRootSet that tracks all active GC roots across all tokio tasks and runtimes.

- **FR-002**: System MUST provide a drop-based GcRootGuard that registers a Gc pointer on creation and unregisters it on drop.

- **FR-003**: System MUST provide Gc::yield_now() method that yields to the tokio scheduler allowing GC to run.

- **FR-004**: System MUST provide Gc::root_guard() method that creates a drop guard for manual root management.

- **FR-005**: System MUST provide #[gc::main] procedural macro that initializes GcRootSet and creates a tokio runtime.

- **FR-006**: System MUST provide #[gc::root] procedural macro that automatically wraps async blocks with root guards.

- **FR-007**: System MUST provide gc::spawn function that wraps tokio::spawn with automatic root tracking.

- **FR-008**: System MUST provide a dirty flag mechanism to notify GC when roots are modified.

- **FR-009**: System MUST support multiple concurrent tokio runtimes sharing a single GcRootSet.

- **FR-010**: System MUST unregister roots automatically when GcRootGuard is dropped.

- **FR-011**: System MUST allow Gc pointers to be safely accessed from spawned tasks when proper root guards are in place.

- **FR-012**: System MUST compile with tokio feature disabled, providing no-op implementations where appropriate.

### Key Entities

- **GcRootSet**: A process-level singleton that maintains the collection of active GC roots. Contains a mutex-protected vector of root pointers, an atomic count of roots, and an atomic dirty flag for GC notification.

- **GcRootGuard**: A drop guard struct that registers a Gc pointer on creation and unregisters it on drop. Ensures roots are tracked for the guard's lifetime.

- **GcTokioExt**: A trait extension for Gc<T> providing tokio-specific methods: root_guard() and yield_now(). Available only when tokio feature is enabled.

- **GcRootScope**: A future wrapper used by gc::spawn that holds both the wrapped future and a root guard, ensuring automatic root tracking for spawned tasks.

- **#[gc::main]**: A procedural macro attribute that transforms an async function to initialize GcRootSet and run within a tokio runtime.

- **#[gc::root]**: A procedural macro attribute that wraps an async block with automatic root guard creation.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Developers MUST be able to complete a basic async Gc usage pattern (create Gc, spawn task, access Gc in task) in under 5 minutes of reading documentation.

- **SC-002**: System MUST support at least 10,000 concurrent async tasks with active Gc root guards without performance degradation exceeding 20% compared to baseline.

- **SC-003**: Root registration and unregistration operations MUST complete in under 1 microsecond each.

- **SC-004**: 100% of Gc objects with active root guards MUST remain valid until their guards are dropped, with zero premature collections in testing.

- **SC-005**: Memory overhead per root MUST be under 32 bytes per active root guard.

- **SC-006**: The dirty flag mechanism MUST enable the GC to skip collection cycles when no roots have changed, reducing unnecessary GC runs by at least 90%.

- **SC-007**: Developers using proc-macro automation MUST write 80% fewer lines of code compared to manual root management for equivalent functionality.

- **SC-008**: All tokio feature tests MUST compile and pass within 30 seconds on standard development hardware.

### Assumptions

- Users have basic familiarity with tokio async/await patterns.
- Users understand the concept of GC roots from garbage collection theory.
- The tokio runtime version will be 1.x (current stable).
- Gc pointers are Send + Sync when their contained type is Send + Sync + Trace.
- Multi-threaded tokio runtime (multi_thread runtime) is the primary target use case.
- LocalSet with spawn_local is not the primary target but should not be broken by the implementation.

### Dependencies

- tokio crate version 1.0+ (optional dependency, enabled via "tokio" feature)
- tokio-util crate version 0.7+ (optional dependency for RT functionality)
- rudo-gc-derive crate for procedural macro implementation
