# Feature Specification: Cross-Thread GC Handle System

**Feature Branch**: `012-cross-thread-gchandle`
**Created**: 2026-02-10
**Status**: Draft
**Input**: User description: "Implement a cross-thread handle system for rudo-gc that allows safe hand-off of GC-managed object references between threads, enabling frameworks like Rvue to schedule UI updates from async threads without requiring the signal types themselves to be Send + Sync"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Async UI Update Scheduling (Priority: P1)

As a framework developer building reactive UI systems, I need to send references to GC-managed objects from async worker threads back to the main UI thread, so that I can schedule state updates without requiring my signal types to implement thread-safe traits.

**Why this priority**: This is the primary use case that justifies the entire feature. Rvue and similar frameworks cannot function without this capability, as their signal inner types contain non-Send references.

**Independent Test**: Can be fully tested by creating a GC object on one thread, creating a cross-thread handle, sending it through a channel to another thread, and successfully resolving it back to a GC reference on the origin thread to perform an update.

**Acceptance Scenarios**:

1. **Given** a GC-managed object created on the UI thread, **When** I create a cross-thread handle to it and send that handle through a channel to a worker thread, **Then** the worker thread can use the handle to communicate back to the UI thread for updates.

2. **Given** a cross-thread handle received on a worker thread, **When** I attempt to resolve it back to a GC reference on that worker thread, **Then** the system prevents this operation and reports an error, forcing me to send the handle back to the origin thread.

3. **Given** a cross-thread handle received on the origin thread, **When** I resolve it to a GC reference, **Then** I can safely access and modify the referenced object.

---

### User Story 2 - Object Lifetime Management (Priority: P1)

As a framework developer, I need cross-thread handles that automatically keep GC objects alive as long as the handle exists, so that objects aren't collected while I'm still using references to them from other threads.

**Why this priority**: Without this guarantee, objects could be prematurely collected when handles exist to them, causing use-after-free bugs and undefined behavior.

**Independent Test**: Can be fully tested by creating a GC object, creating a strong cross-thread handle to it, dropping the original GC reference, forcing a garbage collection cycle, and verifying the object was kept alive by the handle.

**Acceptance Scenarios**:

1. **Given** a strong cross-thread handle to a GC object exists, **When** all direct GC references to that object are dropped, **Then** the object remains alive and can be resolved through the handle.

2. **Given** the last strong cross-thread handle to an object is dropped, **When** a garbage collection cycle runs, **Then** the object becomes eligible for collection if no other roots exist.

---

### User Story 3 - Weak Cross-Thread References (Priority: P2)

As a framework developer, I need weak cross-thread references that allow checking whether an object still exists without preventing its collection, so I can implement "fire-and-forget" patterns where I only update an object if it's still alive.

**Why this priority**: This is a common pattern in reactive systems for avoiding updates to already-cleaned-up components, improving efficiency and avoiding errors.

**Independent Test**: Can be fully tested by creating a GC object, creating a weak cross-thread handle, dropping all strong references, running garbage collection, and verifying the weak handle correctly reports the object as gone.

**Acceptance Scenarios**:

1. **Given** a weak cross-thread handle to a GC object, **When** all strong references (including strong handles) are dropped and GC runs, **Then** the weak handle's liveness check returns false.

2. **Given** a weak cross-thread handle to a live object, **When** I attempt to resolve it on the origin thread, **Then** I receive a valid GC reference to the object.

---

### User Story 4 - Defensive Thread Handling (Priority: P2)

As a framework developer, I need a way to safely attempt resolution without panicking when I'm uncertain which thread I'm running on, so I can gracefully fall back to alternative behaviors rather than crashing.

**Why this priority**: In complex async applications, thread identity may not always be predictable, and graceful degradation is preferable to panics.

**Independent Test**: Can be fully tested by attempting resolution from an unknown thread context and verifying it returns None rather than panicking, enabling the caller to handle this gracefully.

**Acceptance Scenarios**:

1. **Given** a cross-thread handle, **When** I attempt resolution from an unknown thread context, **Then** the operation returns None instead of panicking, allowing me to queue the work for the correct thread.

---

### Edge Cases

- What happens when the origin thread exits while cross-thread handles still exist to its objects?
- How does the system handle multiple handles to the same object from different threads?
- What happens when an object is accessed through multiple cloned handles simultaneously?
- How does the system behave when handle resolution is attempted after explicit unregistration?

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST provide a handle type that implements Send and Sync even when the referenced type does not, allowing safe transfer between threads.

- **FR-002**: The system MUST enforce that handle resolution back to a GC reference can only occur on the thread where the handle was created, preventing access from foreign threads.

- **FR-003**: Strong cross-thread handles MUST keep referenced GC objects alive for the duration of their existence, preventing collection while any strong handle exists.

- **FR-004**: The system MUST provide weak cross-thread handles that track object liveness without preventing collection.

- **FR-005**: The system MUST allow handles to be safely dropped from any thread, not just the origin thread.

- **FR-006**: The system MUST provide a non-panicking resolution method for contexts where thread identity is uncertain.

- **FR-007**: The system MUST allow handles to be explicitly unregistered, making future resolution attempts fail deterministically.

- **FR-008**: Cloning a strong cross-thread handle MUST create an independent root entry that keeps the object alive independently of other clones.

- **FR-009**: The system MUST be compatible with existing GC features including incremental marking and concurrent collection.

### Key Entities

- **Cross-Thread Handle**: An opaque reference token that can be safely sent between threads. It stores no direct access to the referenced object, preventing foreign-thread access. Keeps the object alive through root registration on the origin thread's control block.

- **Weak Cross-Thread Handle**: A non-owning reference that tracks whether the referenced object is still alive. Does not prevent collection but can check liveness and upgrade to a strong reference if the object exists.

- **Origin Thread**: The thread where a handle was created. Handles can only be resolved back to GC references on this thread.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Framework developers can create cross-thread handles to GC objects and safely transfer them between threads without the objects needing to implement Send or Sync.

- **SC-002**: Cross-thread handles can be sent through async channels and thread pools to any thread in the application, enabling reactive patterns in UI frameworks.

- **SC-003**: Resolution enforcement ensures zero accidental cross-thread access to GC objects, preventing data races on non-thread-safe types.

- **SC-004**: Strong handles successfully prevent garbage collection of referenced objects for their entire lifetime, verified through GC cycles during testing.

- **SC-005**: Weak handles correctly report object liveness and return None when the object has been collected, enabling efficient "skip if gone" patterns.

- **SC-006**: Handle operations (creation, clone, drop, resolve) complete within acceptable latency bounds for interactive applications, with resolve being a fast path suitable for frequent calls.

- **SC-007**: The feature integrates seamlessly with existing GC features without introducing deadlocks or race conditions during concurrent marking and sweeping.
