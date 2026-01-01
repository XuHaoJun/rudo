# Feature Specification: Rust Dumpster GC - BiBOP & Mark-Sweep Engine

**Feature Branch**: `001-rust-dumpster-gc`  
**Created**: 2026-01-02  
**Status**: Draft  
**Input**: User description: "Develop a Rust GC similar to dumpster, referencing concepts from the John McCarthy doc regarding BiBOP and Mark-Sweep."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Basic Allocation and Collection (Priority: P1)

As a developer, I want to allocate objects in a garbage-collected heap using a familiar `Gc<T>` API, so that they are automatically reclaimed when they are no longer reachable.

**Why this priority**: this is the fundamental functionality of a Garbage Collector. Without allocation and reclamation, the system provides no value.

**Independent Test**: Can be tested by allocating objects, dropping all references to them (roots), triggering a collection, and verifying the memory is reclaimed (or objects are finalized).

**Acceptance Scenarios**:

1. **Given** a `Gc<i32>` allocated on the heap, **When** the `Gc` handle goes out of scope and `collect()` is called, **Then** the memory occupied by the integer should be marked as free.
2. **Given** two objects referencing each other (a cycle), **When** all external references are dropped and `collect()` is called, **Then** both objects should be reclaimed (proving Mark-Sweep handles cycles).

---

### User Story 2 - Custom Types with Trace (Priority: P1)

As a developer, I want to store custom structs in the GC heap and automatically derive the tracing logic, so that I don't have to manually implement field traversal.

**Why this priority**: Essential for usability. Users need to store complex data structures, not just primitives.

**Independent Test**: Define a struct with `#[derive(Trace)]`, allocate it, and ensure inner GC pointers are followed during collection.

**Acceptance Scenarios**:

1. **Given** a struct `MyNode` containing a `Gc<MyNode>`, **When** I apply `#[derive(Trace)]`, **Then** the system should generate code to visit the inner field during the marking phase.

---

### User Story 3 - BiBOP Memory Layout (Priority: P2)

As a system architect, I want objects significantly different in size to be allocated in different memory segments (BiBOP), so that fragmentation is minimized and allocation is fast (O(1)).

**Why this priority**: This is the core design requirement derived from the "John McCarthy" document to ensure performance and stability.

**Independent Test**: Inspect the internal heap state after allocating objects of different sizes (e.g., 16 bytes vs 64 bytes) to ensure they reside in different segments/pages.

**Acceptance Scenarios**:

1. **Given** requests for `Gc<u64>` and `Gc<[u64; 8]>`, **When** allocated, **Then** they should be placed in appropriate size-class segments (e.g., Class-8/16 and Class-64).

### Edge Cases

- **Allocation Size Exceeds Max Segment**: If a requested `T` is larger than the largest supported Size Class (e.g., > 2048 bytes), the allocator should fall back to a dedicated "Large Object" allocation path (e.g., `alloc::Global`) or panic if not supported in MVP.
- **Zero-Sized Types**: `Gc<()>` or empty structs should handle allocation efficiently (likely not allocating any heap memory or returning a singleton pointer).
- **Out of Memory**: If the heap cannot expand and collection yields no space, the system should strictly define behavior (likely panic or `aborted` similar to Rust's global allocator).
- **Alignment Mismatch**: Allocator must ensure `Segment<SIZE>` respects `align_of::<T>()`.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST implement a **BiBOP (Big Bag of Pages)** allocator where objects are grouped into segments based on their size class.
- **FR-002**: The system MUST implement a **Mark-Sweep** garbage collection algorithm (non-moving) to reclaim unreachable objects.
- **FR-003**: The system MUST provide a `Gc<T>` smart pointer that acts as a handle to the heap-allocated object.
- **FR-004**: The system MUST implement a mechanism to track **Roots**, following the Shadow Stack approach suggested in the design doc (or a safe RAII equivalent).
- **FR-005**: The system MUST provide a `Trace` trait and a procedural macro `#[derive(Trace)]` for auto-implementing the trait on user types.
- **FR-006**: The system MUST use `const generics` to determine size classes at compile time where possible, as per the design doc.
- **FR-007**: The system MUST support **Thread-Local Allocation** (or at least thread-safe global allocation) to allow concurrent usage (if feasible within scope, essentially `Sync` support).
- **FR-008**: The implementation MUST NOT move objects after allocation (Pinning/Stability) to ensure safety with Rust's `&T` references during the object's lifetime.

### Key Entities

- **GlobalHeap**: The central manager of memory segments.
- **Segment<SIZE>**: A block of memory divided into fixed-size slots of `SIZE`.
- **Page Header**: Metadata within a segment containing the Mark Bitmap / Free List.
- **Gc<T>**: User-facing pointer.
- **ShadowStack**: A thread-local structure tracking active Gc roots.

## Assumptions

- The project will focus on the "Scheme A" (Immobile/Mark-Sweep) approach from the design doc, as moving objects in Rust is unsafe/complex without a handle-based indirection system that sacrifices performance.
- We will prioritize correctness and basic BiBOP implementation over advanced features like concurrent marking for the MVP.
- "Similar to dumpster" refers to the API ergonomics (`derive(Trace)`) rather than the internal `Rc` cycle-detection implementation.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A cycle of objects (A -> B -> A) is successfully collected when no longer reachable from the stack.
- **SC-002**: Allocation of a small object (e.g., `u64`) takes O(1) time in the common case (bump pointer in a segment).
- **SC-003**: The library compiles on stable Rust (unless specific experimental features are strictly required, but preference is stable).
- **SC-004**: Users can define a struct with `derive(Trace)` and use it in `Gc` without writing unsafe code.
