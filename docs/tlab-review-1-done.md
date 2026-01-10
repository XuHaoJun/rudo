# TLAB Review Results

Based on the design document `docs/2026-01-10_08-00-31_Gemini_Google_Gemini.md:1788-2077`, here is the review of the current implementation in `crates/rudo-gc`.

## 1. Core Data Structures
- **Global Heap**: 
    - **Design**: `GlobalHeap` with `Mutex<GlobalHeapInner>`.
    - **Implementation**: Implemented as `GlobalSegmentManager` (in `heap.rs`), managed via a global `OnceLock<Mutex<GlobalSegmentManager>>`. It correctly handles the "Slow Path" of requesting fresh pages from the OS.
- **Page Header**:
    - **Design**: `PageHeader` with metadata and bitmaps.
    - **Implementation**: Fully implemented in `heap.rs` (lines 58-84). It includes the required `magic`, `block_size`, and bitmaps (`mark`, `dirty`, `allocated`).
- **Local Context**:
    - **Design**: `ThreadLocalContext` aggregating all size class cursors.
    - **Implementation**: Implemented as `LocalHeap` (in `heap.rs`), which contains 8 `Tlab` instances (for sizes 16 to 2048).

## 2. TLAB & Fast Path Allocation
- **Fast Path**:
    - **Design**: Bump pointer check (`ptr` + `size` <= `limit`).
    - **Implementation**: Implemented in `Tlab::alloc` (in `heap.rs`). It performs the bump pointer increment and, notably, updates the `allocated_bitmap` directly to ensure GC visibility.
- **Slow Path (Refill)**:
    - **Design**: `alloc_slow` to refill from `GlobalHeap`.
    - **Implementation**: Implemented in `LocalHeap::alloc_slow`. It requests a page from `GlobalSegmentManager`, initializes the header, and sets up the new bump pointers in the Tlab.
- **Routing**:
    - **Design**: 32 size classes suggested.
    - **Implementation**: Uses 8 power-of-two size classes (16, 32, ..., 2048). Routing is handled via `compute_size_class` and `compute_class_index`.

## 3. BiBOP (Big Bag of Pages)
- **Memory Layout**: 
    - **Design**: 4KB pages, each with objects of a single size class.
    - **Implementation**: Matches the design. `PAGE_SIZE` is 4KB, and `ptr_to_page_header` uses bitmasking (`PAGE_MASK`) for O(1) metadata lookup.
- **Fragmentation**: 
    - **Design**: TLAB for fresh pages, Free List for fragmented pages.
    - **Implementation**: `LocalHeap::alloc` first tries the TLAB, then falls back to `alloc_from_free_list` (which uses the `free_list_head` in `PageHeader`) before triggering `alloc_slow` for a new page. This perfectly matches the hybrid strategy.

## 4. GC Handshake & Coordination
- **Flush Mechanism**:
    - **Design**: `flush_segment` called at GC time to "retire" the TLAB.
    - **Implementation**: Instead of an explicit flush, `Tlab::alloc` marks the `allocated_bitmap` on every allocation. Since the page is already registered in `LocalHeap::pages`, the collector sees it automatically.
- **Thread Safety**:
    - **Design**: Suggested `UnsafeCell` for performance and a global registry for STW.
    - **Implementation**: Current implementation uses `thread_local! { RefCell<LocalHeap> }`. This is safe but slightly slower than the proposed `UnsafeCell` approach. The global registry and STW handshake (poll check) are currently missing.

## Summary Table

| Feature | Design | Status | Note |
| :--- | :--- | :--- | :--- |
| **BiBOP Layout** | 4KB Pages + Size Classes | ✅ Implemented | Uses 8 classes instead of 32. |
| **Fast Path** | Bump Pointer (Lock-free) | ✅ Implemented | `Tlab::alloc` is inlined. |
| **Slow Path** | Refill from Global Heap | ✅ Implemented | `LocalHeap::alloc_slow`. |
| **Fragmentation** | Linear + Free List Hybrid | ✅ Implemented | Uses both TLAB and `free_list_head`. |
| **UnsafeCell TLS** | Proposed for performance | ⚠️ Deviation | Uses `RefCell` for safety. |
| **GC Handshake** | STW Registry / Poll Check | ❌ Missing | Not yet required for single-threaded. |

Overall, the core allocation engine and memory layout match the Gemini design very closely, particularly the BiBOP and TLAB refill logic.
