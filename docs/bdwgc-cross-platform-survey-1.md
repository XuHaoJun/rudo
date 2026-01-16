# BDWGC Cross-platform Stack Bounds Detection Survey

This document investigates how the Boehm-Demers-Weiser Garbage Collector (BDWGC) handles stack bounds detection across various platforms. This survey is intended to inform the implementation of cross-platform stack bounds detection in `rudo`.

## Core Mechanisms

In BDWGC, the primary entry point for obtaining the stack base (the "cold" end of the stack) is `GC_get_stack_base()` and `GC_get_main_stack_base()`, implemented in `os_dep.c`.

### 1. Windows (MSVC, MinGW, Cygwin)

BDWGC uses multiple strategies for Windows depending on the specific environment:

- **Generic Windows (ANY_MSWIN)**:
  - Uses `VirtualQuery()` on the current stack pointer.
  - It probes the memory region containing the stack pointer to find its extent.
  - Implementation in `os_dep.c`:
    ```c
    trunc_sp = PTR_ALIGN_DOWN(GC_approx_sp(), GC_page_size);
    size = GC_get_writable_length(trunc_sp, 0); // Calls VirtualQuery
    sb->mem_base = trunc_sp + size;
    ```
- **Cygwin**:
  - Accesses the **Thread Environment Block (TEB)** directly.
  - For x86_64: `((NT_TIB *)NtCurrentTeb())->StackBase`
  - For x86: Reads the stack base from the segment register `fs:4`.

### 2. macOS (Darwin)

macOS provides a specific pthread extension for this:

- **Primary Method**: `pthread_get_stackaddr_np(pthread_self())`.
- According to BDWGC comments, this returns the "stack bottom" (the highest stack address plus 1).
- This is available even in single-threaded builds on Darwin.

### 3. Linux and Modern Unix (pthreads)

For platforms supporting `pthread_getattr_np` (GNU extension) or `pthread_attr_get_np` (FreeBSD/Solaris):

- **Method**:
  1. `pthread_getattr_np(pthread_self(), &attr)`
  2. `pthread_attr_getstack(&attr, &addr, &size)`
  3. Base is `addr + size` (for downward growing stacks).
- **Linux Main Thread Fallback**: If the above fails for the main thread, BDWGC may read `/proc/self/stat` and parse the 28th field (`startstack`).

### 4. Other Unix Variants

- **FreeBSD**: Uses `sysctl` with `KERN_USRSTACK`.
- **OpenBSD**: Uses `pthread_stackseg_np()`.
- **Solaris**: Uses `thr_stksegment()`.
- **QNX**: Uses `__builtin_frame_address(0)` as a fallback (less exact).

### 5. Heuristic Fallback (Generic Unix)

If no platform-specific API is available, BDWGC employs a "find limit" strategy:
- It probes memory addresses starting from the current stack pointer in the direction of the stack base.
- It installs a temporary `SIGSEGV` (and `SIGBUS`) handler.
- It uses `setjmp()` / `longjmp()` to recover from faults when it hits an unmapped page.
- This is considered a last resort and can be slow or risky (e.g., if there's no guard page between stack and other regions).

## Implications for `rudo`

For `rudo` to support macOS and Windows, the following APIs should be used in `stack.rs`:

| Platform | Recommended API | Notes |
| :--- | :--- | :--- |
| **macOS** | `pthread_get_stackaddr_np` | Direct and reliable. |
| **Windows** | `GetCurrentThreadStackLimits` | Modern Windows API (Vista+). BDWGC uses `VirtualQuery` which is more compatible with very old Windows but `GetCurrentThreadStackLimits` is cleaner for modern versions. |
| **Linux** | `pthread_getattr_np` | Already partially used or can be used as a more robust alternative to parsing `/proc`. |

### Proposed `stack.rs` Implementation Strategy

```rust
#[cfg(target_os = "macos")]
fn get_stack_bounds() -> StackBounds {
    // Call pthread_get_stackaddr_np to get high address
    // Use pthread_get_stacksize_np to get size and calculate low address
}

#[cfg(target_os = "windows")]
fn get_stack_bounds() -> StackBounds {
    // Call GetCurrentThreadStackLimits
}
```
