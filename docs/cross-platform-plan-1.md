# Cross-Platform Implementation Plan

**Authors**: R. Kent Dybvig & John McCarthy (Parallel World Collaboration)
**Date**: 2026-01-16
**Status**: Draft

## Executive Summary

We have analyzed the `rudo-gc` substrate and identified the necessary transformations to extend its domain to AArch64, macOS, and Windows. Our approach balances the rigorous, low-level efficiency required for a collector (Dybvig's influence) with the desire for elegant, generalized abstractions (McCarthy's influence).

The immediate priority is to ensure the mutator can operate correctly on ARM architectures and non-Linux operating systems by abstracting stack and register access.

## Phase 1: The Ephemeral State (Registers & Stack)

The collector must traverse the roots of computation, which reside in the machine registers and the control stack. Currently, this is coupled to `x86_64` and Linux.

### 1.1 Generic Register Spilling (The "setjmp" Abstraction)
**Goal**: Enable AArch64 and RISC-V support without hand-written assembly for every target.

*   **Analysis**: Writing assembly for every architecture is brittle. The standard C library function `setjmp` preserves callee-saved registers into a buffer. By inspecting this buffer (or simply ensuring it is on the stack and scanning the stack), we catch the roots.
*   **Plan**:
    *   Modify `crates/rudo-gc/src/stack.rs`.
    *   Introduce a `generic` module for non-`x86_64` targets.
    *   Use the `setjmp` idiom (via `libc` crate) to spill registers to the stack.
    *   *Note*: We must ensure the `jmp_buf` is scanned. In most implementations, `setjmp` stores registers in the buffer *on the stack* or spills them to the stack frame. We will verify that `setjmp` + conservative stack scan covers the registers.
    *   **Optimization**: For `aarch64`, we may eventually add explicit ASM (storing `x19-x29`) for precision, but `setjmp` suffices for bootstrapping.

### 1.2 Register Clearing
**Goal**: Prevent false roots from dead registers.

*   **Plan**:
    *   Implement a generic `clear_registers` using a compiler fence or a dummy function call that clobbers registers.
    *   For `aarch64`, implement specific ASM to zero `x0-x18` (caller-saved) if needed, though usually allocator return paths handle this. The critical part is clearing registers *before* allocation to avoid retaining old pointers.

### 1.3 Stack Bounds Discovery
**Goal**: Correctly identify the mutator's stack range on macOS and Windows.

*   **Plan**:
    *   **macOS**: Implement `get_stack_bounds` using `pthread_get_stackaddr_np` (and `pthread_get_stacksize_np`).
    *   **Windows**:
        *   Introduce `windows-sys` dependency to `rudo-gc`.
        *   Use `VirtualQuery` on the address of a local variable to find the `AllocationBase` (stack bottom) and region size.
        *   Alternatively, access the TEB (Thread Environment Block) if `VirtualQuery` proves too slow, but `VirtualQuery` is robust.

## Phase 2: The Heap Substrate (Memory Allocation)

The heap assumes a rigid 4KB page size, which is mathematically inelegant on systems with larger granularity (Windows 64KB).

### 2.1 Dynamic Page Size
**Goal**: Decouple `rudo-gc` from the compile-time `PAGE_SIZE` constant.

*   **Analysis**: `rudo-gc` uses `PAGE_SIZE` (4096) for BiBOP layout. On Windows, `sys_alloc` aligns to 64KB. Requesting 4KB results in 64KB consumption (60KB wasted).
*   **Plan**:
    *   Refactor `heap.rs`:
        *   Replace `const PAGE_SIZE` with a static `PAGE_SIZE` initialized at runtime (via `sys_alloc::allocation_granularity()`).
        *   Replace `PAGE_MASK` with a runtime-calculated mask.
    *   Update `SizeClass` logic to respect the runtime page size.
    *   **Windows Specific**: Ensure we request 64KB pages to achieve 1:1 mapping with OS allocation granules.

### 2.2 Windows Memory Mapping
**Goal**: robust `sys_alloc` on Windows.

*   **Status**: `crates/sys_alloc/src/windows.rs` appears largely correct but needs verification against `rudo-gc`'s usage patterns.
*   **Plan**:
    *   Verify `VirtualAlloc` flags (`MEM_RESERVE | MEM_COMMIT`).
    *   Ensure error handling propagates correctly to the `GlobalSegmentManager`.

## Implementation Roadmap

1.  **Step 1 (Stack/Regs)**: Modify `stack.rs` to add `setjmp` fallback and macOS/Windows stack bounds. (Estimated: 1 day)
2.  **Step 2 (Page Size)**: Refactor `heap.rs` to support dynamic `PAGE_SIZE`. (Estimated: 2 days)
3.  **Step 3 (CI/Tests)**: Add cross-compilation checks (or at least `cargo check --target ...`) to CI for `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`.

## Signatures

*R. Kent Dybvig*
*John McCarthy*
