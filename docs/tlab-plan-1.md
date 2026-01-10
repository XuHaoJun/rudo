# TLAB Implementation Plan for Rudo GC

This plan outlines the architecture for a "Chez Scheme-inspired" **Thread-Local Allocation Buffer (TLAB)** system within `rudo-gc`.

## 1. Objectives
*   **Zero-Lock Fast Path**: Allocation in the common case must be a simple pointer-bump without atomic operations or mutexes.
*   **Memory Efficiency**: Move from "Thread-Local Global Heaps" to a "Shared Segment Manager" to reduce memory fragmentation and overhead across multiple threads.
*   **Cache Locality**: Ensure that objects allocated by the same thread are close to each other in memory.
*   **Generational Compatibility**: Seamlessly integrate with the existing minor/major collection logic.

## 2. Refactoring Architecture

### Current State
*   `GlobalHeap` is stored in a `thread_local!`.
*   Each `GlobalHeap` owns its own segments and performs its own `mmap` calls.
*   Wasted memory: If 10 threads exist, we allocate at least 10 segments for the 16-byte size class, even if some threads rarely allocate.

### Proposed State
1.  **`GlobalSegmentManager` (Singleton)**:
    - Shared across all threads (wrapped in `OnceLock` and `Mutex`).
    - Owns the physical `Mmap` regions.
    - Hands out 4KB `Pages` to threads on request.
2.  **`ThreadContext` (Thread-Local)**:
    - Renamed from current `GlobalHeap`.
    - Holds the **TLABs** for each size class.
    - Requests new pages from `GlobalSegmentManager` when a TLAB is exhausted.
3.  **`TLAB` Structure**:
    ```rust
    struct Tlab {
        current: *mut u8,
        end: *const u8,
        page_header: *mut PageHeader,
    }
    ```

## 3. Implementation Steps

### Phase 1: Shared Segment Manager
*   Implement `GlobalSegmentManager` in `heap.rs`.
*   Use a `Mutex<Vec<NonNull<PageHeader>>>` to track free pages.
*   Implement `request_page(size_class) -> NonNull<PageHeader>`.

### Phase 2: TLAB Integration
*   Modify `Segment` in `GlobalHeap` (now `LocalHeap`) to behave like a TLAB.
*   Instead of `pages: Vec<...>`, it should only hold the *currently active page* and a list of *fully exhausted pages*.
*   When `bump_ptr == bump_end`, call the `GlobalSegmentManager::request_page`.

### Phase 3: Fast Path Optimization
*   Mark `LocalHeap::alloc` as `#[inline(always)]`.
*   Optimize `Gc::new` to directly access the thread-local TLAB with minimal branching.
*   **Register Hinting (Experimental)**: Investigate if we can use `thread_local!` macros that leverage native TLS (e.g., `#[thread_local]` in nightly Rust) to minimize `RefCell` overhead.

### Phase 4: Generational Tracking
*   Ensure that when a Page is handed back to the `GlobalSegmentManager` (after collection), its `dirty_bitmap` and `mark_bitmap` are correctly reset.
*   Update the `RemSet` logic to account for TLAB-localized allocations.

## 4. Performance Expectations
| Metric | Current (Global-in-TLS) | Target (TLAB + Shared Manager) |
| :--- | :--- | :--- |
| Allocation (Fast Path) | ~5-10ns | ~2-4ns |
| Page Acquisition | Low (Direct `mmap`) | Medium (Mutex lock) |
| Memory Overhead | High (N threads * 8 classes) | Low (Global Pool) |

## 5. Risk Assessment
*   **Lock Contention**: If many threads request pages simultaneously, the `GlobalSegmentManager` could become a bottleneck. *Mitigation*: Request multiple pages at once ("Chunking").
*   **Provenance/Miri**: Handling raw pointer spans across a global manager will require careful use of `expose_provenance` or keeping track of the original `Mmap` objects.
