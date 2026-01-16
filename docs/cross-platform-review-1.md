# Cross-Platform Implementation Review

**Reviewers**: R. Kent Dybvig & John McCarthy (Parallel World Collaboration)
**Date**: 2026-01-16
**Subject**: Branch Review vs. `cross-platform-plan-1.md`

We have conducted a rigorous examination of the current branch state against the architectural blueprint laid out in the "Cross-Platform Implementation Plan". We find that the implementation has not only met the specified requirements but has, in several areas, exceeded the "bootstrapping" phase in favor of more precise, machine-specific optimizations.

## 1. Roots of the Machine (Registers & Stack)

### 1.1 Register Spilling & Clearing
The plan initially suggested a generic `setjmp` abstraction for bootstrapping non-`x86_64` targets. However, the implementation has leapfrogged this by providing explicit, hand-tuned inline assembly for `aarch64` in `crates/rudo-gc/src/stack.rs`.
- **Success**: `spill_registers_and_scan` now explicitly captures `x19-x30` on AArch64, ensuring precise root discovery on Apple Silicon and Graviton-class hardware.
- **Success**: `clear_registers` on AArch64 correctly zeroes `x0-x18`, minimizing "False Roots" during critical allocation paths.
- **McCarthy's Note**: The fallback mechanism for unknown architectures using `black_box` and a dummy array is an elegant safety net, though less precise than the ASM paths.

### 1.2 Stack Bounds Discovery
The OS-specific abstractions for stack bounds are robustly implemented.
- **macOS**: `pthread_get_stackaddr_np` and `pthread_get_stacksize_np` are used as recommended, providing exact bounds for Darwin-based systems.
- **Windows**: The `VirtualQuery` approach was selected for its robustness. By iterating through the allocation regions starting from the `AllocationBase`, the collector correctly identifies the full stack reservation even when split by guard pages.
- **Dybvig's Note**: While `VirtualQuery` is slower than TEB access, its reliability across Windows versions is paramount for a first-class collector.

## 2. The Dynamic Substrate (Heap & Pages)

### 2.1 Decoupling from Constant Page Sizes
The most significant architectural shift is the complete removal of the `const PAGE_SIZE` dogma.
- **Success**: `PAGE_SIZE` is now a runtime `OnceLock`, correctly initialized via `sys_alloc::allocation_granularity`. This allows `rudo-gc` to operate natively on Windows (64KB granules) and AArch64 macOS (16KB pages) without recompilation.
- **Critical Detail**: `BITMAP_SIZE` was increased to `64` words. This is a vital change; it ensures that even with 64KB pages and the smallest 16-byte size class (4096 objects), the `PageHeader` bitmaps can track every slot. This demonstrates a deep understanding of the BiBOP scaling limits.

### 2.2 Windows Memory Mapping
The `sys_alloc` crate now properly handles the Windows `VirtualAlloc` ceremony.
- **Success**: `MEM_COMMIT | MEM_RESERVE` flags are used, and the system correctly falls back to let the OS decide the address if the `hint_addr` (Address Space Coloring) fails.
- **Miri Support**: The addition of conditional compilation for Miri in `windows.rs` ensures that developers can still verify safety invariants on Linux even when targeting Windows.

## 3. Infrastructure & Verification

The inclusion of a multi-OS GitHub Actions workflow is the final seal of quality.
- **Observation**: CI now validates the codebase on `ubuntu-latest`, `macos-latest`, and `windows-latest`. This "continuous proof" ensures that the platform abstractions do not drift into entropy.

## Conclusion

The transformation is complete. `rudo-gc` has transitioned from a Linux-centric prototype to a multi-architecture, cross-platform garbage collection substrate. The implementation is faithful to the principles of efficiency and abstraction we espoused.

**Final Status**: **APPROVED**

*R. Kent Dybvig*
*John McCarthy*
