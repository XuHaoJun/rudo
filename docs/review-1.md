# Rudo GC Implementation Review

**Reviewer**: Antigravity  
**Date**: 2026-01-02  
**Target Crate**: `crates/rudo-gc`  
**Reference Plan**: `docs/2026-01-01_22-27-34_Gemini_Google_Gemini.md` (John McCarthy's "Plan A")

## 1. Overview

The `rudo-gc` crate implements a **Non-moving Mark-Sweep Garbage Collector** using a **BiBOP (Big Bag Of Pages)** memory allocator. This aligns with "Plan A" proposed in the reference document, which prioritized address stability (`&T` compatibility) over the moving/compacting nature of Chez Scheme's GC for Rust.

## 2. Architecture Verification

The implementation successfully executes the core "Lisp-on-Rust" architecture:

### 2.1 Memory Layout: BiBOP (Confirmed)
- **Implementation**: `crates/rudo-gc/src/heap.rs`
- **Verification**: The heap is organized into `Segment<BLOCK_SIZE>` structures. Pages are 4KB aligned. Each page contains objects of a single size class (16, 32, 64, ..., 2048 bytes). Large objects (>2048 bytes) are handled separately in a Large Object Space (LOS).
- **Match with Plan**: **Perfect Match**. The implementation uses the exact strategy described: homogeneous pages to avoid per-object headers and enable O(1) metadata lookup.

### 2.2 Allocation: Compile-Time Size Classes (Confirmed)
- **Implementation**: `crates/rudo-gc/src/heap.rs` (`trait SizeClass`)
- **Verification**: The allocator uses Rust's `const` generics and `const fn` to route allocations to the correct segment size class at compile time (or effectively so via optimization of the `match` block).
- **Match with Plan**: **High**. It utilizes Rust's type system to resolve size classes efficiently as requested.

### 2.3 Algorithm: Non-Moving Mark-Sweep (Confirmed)
- **Implementation**: `crates/rudo-gc/src/gc.rs`
- **Verification**: The collector performs a standard Mark-Sweep.
    - **Mark**: Traces from roots (stack) and sets bits in the `PageHeader` bitmap.
    - **Sweep**: Scans bitmaps; unmarked slots are added to a free list within the page. Objects are *not* moved.
- **Match with Plan**: **Perfect Match (Plan A)**. This respects Rust's pinning/reference stability requirements.

## 3. Key Deviations & Implementation Details

### 3.1 Root Finding: Conservative Stack Scanning
- **Plan Suggestion**: The plan recommended a "Shadow Stack" (via RAII guards) as the "safest pure Rust implementation", with Conservative Scanning as a secondary option.
- **Actual Implementation**: **Conservative Stack Scanning** (`crates/rudo-gc/src/stack.rs`).
    - The implementation spills registers (`spill_registers_and_scan`) and scans the stack memory for values that look like pointers into the GC heap.
    - **Implication**: This is simpler to use (no explicit `Root<T>` handles needed), but relies on platform-specific stack bounds (implemented for **Linux** via `pthread_getattr_np`). Miri support is stubbed out.

### 3.2 Generational GC: Not Implemented
- **Plan Suggestion**: "Algorithm: Non-Moving Generational".
- **Actual Implementation**: **Single-Generation**.
    - `PageHeader` contains a `generation` field, but it is currently unused. The GC performs a full collection every time.
    - **Write Barriers**: No write barriers (`DerefMut` interception) are implemented.
    - **Status**: This effectively represents "Phase 1" of the plan.

## 4. Code Quality & Safety

- **Safety Comments**: The code makes extensive use of `unsafe` (inherent to GC implementation) but includes `// SAFETY:` comments explaining the rationale.
- **Documentation**: The crate is well-documented with rustdoc comments explaining the BiBOP layout and algorithms.
- **Tests**: Contains basic logic verification. Miri tests are present but inline assembly is disabled for Miri (as expected).

## 5. Conclusion

The `rudo-gc` crate is a **faithful implementation of the fundamental "Plan A" architecture**. It successfully marries Rust's static typing (for allocator routing) with Chez Scheme's memory layout (BiBOP).

**Grade: A-**
( deducted for absence of Generational/Write Barrier mechanism promised in the more advanced sections of the plan, and Linux-only constraint for stack scanning).

### Recommendations for Next Steps
1.  **Implement Write Barriers**: To enable Generational GC usage (scanning only young generation pages usually).
2.  **Cross-Platform Stack Bounds**: Add support for macOS/Windows stack bounds retrieval to make the "Conservative Scanning" portable.
3.  **Shadow Stack Mode**: Consider adding an optional "Shadow Stack" mode for platforms where stack scanning is unsafe or unsupported.
