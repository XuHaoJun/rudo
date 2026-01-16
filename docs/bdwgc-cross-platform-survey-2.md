# BDWGC Cross-Platform Survey Results

This document summarizes the investigation into the Boehm-Demers-Weiser Garbage Collector (BDWGC) codebase, specifically analyzing how it handles cross-platform challenges identified in `cross-platform-issue-1.md`.

## 1. Stack & Register Management

### A. Register Spilling (`spill_registers_and_scan`)
**Issue:** `rudo-gc` currently only supports x86_64 via inline assembly.
**BDWGC Approach:**
- **Generic Fallback:** BDWGC minimizes the need for per-architecture assembly by using generic C alternatives in `mach_dep.c`:
  - `__builtin_unwind_init()`: Used where available to force callee-saved registers onto the stack.
  - `setjmp` / `_setjmp`: Used as a portable way to save registers to a `jmp_buf` on the stack.
- **Architecture Specifics:**
  - **IA64:** Uses a specific assembly file `ia64_save_regs_in_stack.s`.
  - **macOS (AArch64):** In `darwin_stop_world.c`, it explicitly accesses registers of *suspended* threads using `thread_get_state` and manually pushes `x0-x28`, `fp`, and `lr`.
  - **Linux (AArch64):** Appears to rely on the generic `__builtin_unwind_init` or `setjmp` mechanism for the current thread, and standard signal handling contexts for suspended threads.

**Recommendation for rudo-gc:**
- Implement a generic fallback using `setjmp` for architectures lacking explicit assembly support. This provides immediate support for AArch64 and RISC-V without writing assembly for each.
- For macOS, `thread_get_state` is the verified way to access registers of suspended threads.

### B. Stack Bounds Detection
**Issue:** `rudo-gc` only supports Linux (`pthread_getattr_np`).
**BDWGC Approach:**
- **Linux:** Uses `/proc/self/maps` parsing in `os_dep.c` (`GC_get_maps`) to identify stack segments if other methods fail.
- **Windows:** Uses `VirtualQuery` on the stack pointer or accesses the Thread Environment Block (TEB) via `NtCurrentTeb()->StackBase` in `os_dep.c` (`GC_get_stack_base`).
- **macOS:**
  - **Walking the Stack:** In `darwin_stop_world.c`, `GC_FindTopOfStack` uses inline assembly (reading `x29` on AArch64) to walk stack frames upwards to find the base. This is a robust, albeit low-level, alternative to `pthread_get_stackaddr_np`.
  - **Mach Threads:** For other threads, it tracks stack ranges via thread creation hooks (`p->crtn->stack_end`).

**Recommendation for rudo-gc:**
- **Windows:** Implement `VirtualQuery` based discovery or TEB access (requires `use-std` or `windows-sys` crate features).
- **macOS:** `pthread_get_stackaddr_np` is the modern standard, but BDWGC's frame walking demonstrates a fallback if the POSIX API is insufficient.

## 2. Low-level Memory Allocation

### A. Windows Support (`VirtualAlloc`)
**Issue:** Need to handle `VirtualAlloc` and 64KB vs 4KB page vs allocation granularity.
**BDWGC Approach:**
- **Implementation:** Uses `VirtualAlloc` in `os_dep.c` with `MEM_COMMIT`, `MEM_RESERVE`, and `MEM_TOP_DOWN`.
- **Granularity Handling:** Explicitly sets `GC_page_size` to `dwAllocationGranularity` (typically 64KB) instead of `dwPageSize` (4KB) on Windows (see `GC_setpagesize`).
  ```c
  GC_page_size = (size_t)GC_sysinfo.dwAllocationGranularity;
  ```
- This simplifies internal logic by treating the system as having larger pages, ensuring all `mmap`/`VirtualAlloc` calls are aligned to the OS requirement.

**Recommendation for rudo-gc:**
- Align `rudo-gc`'s "Page" concept to `sys_alloc::page_size()` at runtime.
- On Windows, strictly use `GetSystemInfo` to determine `dwAllocationGranularity` and treat that as the minimum block size for OS allocations, preventing invalid `VirtualAlloc` calls.

### B. Dynamic Page Size
**Issue:** `heap.rs` assumes constant 4096.
**BDWGC Approach:**
- **Runtime Configuration:** `GC_setpagesize()` initializes a global `GC_page_size` variable at startup using `sysconf(_SC_PAGESIZE)` (Unix) or `GetSystemInfo` (Windows).
- **Usage:** All allocator logic uses this global variable rather than a compile-time constant.

**Recommendation for rudo-gc:**
- Refactor `heap.rs` constants (`PAGE_SIZE`) into a runtime-initialized static or `lazy_static` value.
- Ensure all pointer arithmetic usually relying on `Assuming 4k alignment` is updated.

## 3. Multi-threading & Coordination

### A. Thread Suspension
**Issue:** Need generic suspension for Unix and Windows.
**BDWGC Approach:**
- **Unix:** Uses a signal-based handshake (`SIGRTMIN` or `SIGPWR`).
  - `pthread_kill` sends the signal.
  - Signal handler uses `sigsuspend` to wait until resumed.
- **Windows:** Uses `SuspendThread` and `ResumeThread` API in `win32_threads.c`.
- **macOS:** Uses Mach generic logic (`task_threads`, `thread_suspend`) in `darwin_stop_world.c`.

**Recommendation for rudo-gc:**
- The current safepoint implementation is good. For preemptive stopping:
  - **Windows:** `SuspendThread` is the reliable path.
  - **Unix:** Signals are standard (BDWGC proves viability).
  - **macOS:** Mach APIs are required for robust suspension (standard POSIX signals are often insufficient for perfect "stop-the-world" without cooperation).

## Summary of Actionable Items

| Component | Architecture/OS | Action Item from BDWGC |
| :--- | :--- | :--- |
| **Register Spilling** | All | Implement `setjmp` based register spilling as a generic fallback. |
| **Stack Bounds** | Windows | Use `VirtualQuery` or TEB (`NtCurrentTeb`). |
| **Memory Alloc** | Windows | align allocations to `dwAllocationGranularity` (64KB). |
| **Stack Bounds** | macOS | Consider frame-walking if `pthread_get_stackaddr_np` fails (generic `GC_FindTopOfStack`). |
