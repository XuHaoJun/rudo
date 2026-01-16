# rudo-gc Cross-platform Support Issues

This document tracks identified areas in the `rudo-gc` codebase that require additional cross-platform or cross-architecture support.

## 1. Stack & Register Management (`stack.rs`)

Conservative stack scanning requires interaction with CPU registers and stack layouts, which are highly architecture-specific.

### A. Register Spilling (`spill_registers_and_scan`)
- **Current State**: Only implemented for `x86_64` using inline assembly.
- **Requirement**: Implement assembly blocks for `aarch64` (ARM64), `riscv64`, etc.
- **File**: `crates/rudo-gc/src/stack.rs`
- **Priority**: **High** (Necessary for Apple Silicon and ARM servers).

### B. Register Clearing (`clear_registers`)
- **Current State**: Only clears `R12-R15` on `x86_64`.
- **Requirement**: Implement zeroing of callee-saved registers for other architectures to prevent "False Roots" (stale pointers keeping objects alive).
- **File**: `crates/rudo-gc/src/stack.rs`
- **Priority**: **Medium** (Affects collection precision).

### C. Stack Bounds Detection (`get_stack_bounds`)
- **Current State**: Only implemented for Linux via `pthread_getattr_np`.
- **Requirement**:
  - **macOS**: Use `pthread_get_stackaddr_np`.
  - **Windows**: Use `GetCurrentThreadStackLimits`.
- **File**: `crates/rudo-gc/src/stack.rs`
- **Priority**: **High** (Core requirement for cross-OS support).

## 2. Low-level Memory Allocation (`sys_alloc`)

The garbage collector manages its own memory segments, requiring direct OS calls for memory mapping.

### A. Windows Support
- **Current State**: `windows.rs` exists but needs full implementation of `VirtualAlloc` / `VirtualFree`.
- **Requirement**: Handle Windows-specific "Allocation Granularity" (typically 64KB) vs "Page Size" (4KB).
- **File**: `crates/sys_alloc/src/windows.rs`
- **Priority**: **High**.

### B. Dynamic Page Size
- **Current State**: Many parts of `heap.rs` assume a constant `4096` byte page size.
- **Requirement**: Transition to using `sys_alloc::page_size()` at runtime. Some architectures (e.g., AArch64 on macOS) use 16KB pages.
- **File**: `crates/rudo-gc/src/heap.rs`
- **Priority**: **Medium**.

## 3. Multi-threading & Coordination (`heap.rs`, `gc.rs`)

Future support for multi-threaded GC or concurrent marking will introduce more OS dependencies.

### A. Thread Suspension
- **Requirement**:
  - **Unix**: Signal-based suspension (`pthread_kill`).
  - **Windows**: API-based suspension (`SuspendThread` / `ResumeThread`).
- **Priority**: **Low** (Current implementation uses cooperative safepoints).

### B. Signal Handling / Fault Probing
- **Requirement**: If implementing hardware-based read/write barriers, OS-specific signal handler setups (and parsing `ucontext_t`) will be needed.
- **Priority**: **Low**.

## 4. Tooling & Scripts

### A. Shell Scripts
- **Current State**: `test.sh`, `clippy.sh`, `miri-test.sh` are Bash scripts.
- **Requirement**: Provide PowerShell equivalents for Windows or migrate to `cargo-xtask`.
- **Priority**: **Low**.

## Summary Table

| Category | Component | Target | Priority |
| :--- | :--- | :--- | :--- |
| **Architecture** | Register Spilling | AArch64, ARM | High |
| **OS** | Stack Bounds | macOS, Windows | High |
| **OS** | Memory Mapping | Windows | High |
| **Architecture** | Dynamic Page Size | AArch64 | Medium |
| **Tooling** | Test Scripts | Windows | Low |
