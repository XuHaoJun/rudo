//! Conservative stack scanning for root tracking.
//!
//! This module provides utilities to spill CPU registers onto the stack
//! and scan the stack for potential pointers into the GC heap.

/// Bounds of a thread's stack.
#[derive(Debug, Clone, Copy)]
pub struct StackBounds {
    /// The bottom of the stack (highest address).
    pub bottom: usize,
    /// The top of the stack (lowest address).
    #[allow(dead_code)]
    pub top: usize,
}

/// Retrieve the stack bounds for the current thread.
#[cfg(miri)]
pub fn get_stack_bounds() -> StackBounds {
    // Miri does not support stack scanning or direct access to the stack bounds.
    // Return a dummy range that results in no scanning.
    StackBounds { bottom: 0, top: 0 }
}

/// Retrieve the stack bounds for the current thread.
#[cfg(all(target_os = "linux", not(miri)))]
pub fn get_stack_bounds() -> StackBounds {
    use libc::{
        pthread_attr_destroy, pthread_attr_getstack, pthread_attr_t, pthread_getattr_np,
        pthread_self,
    };

    unsafe {
        let mut attr: pthread_attr_t = std::mem::zeroed();
        let ret = pthread_getattr_np(pthread_self(), &raw mut attr);
        assert!(ret == 0, "pthread_getattr_np failed");

        let mut stackaddr: *mut libc::c_void = std::ptr::null_mut();
        let mut stacksize: libc::size_t = 0;
        let ret = pthread_attr_getstack(&raw const attr, &raw mut stackaddr, &raw mut stacksize);
        if ret != 0 {
            pthread_attr_destroy(&raw mut attr);
            panic!("pthread_attr_getstack failed");
        }
        pthread_attr_destroy(&raw mut attr);

        let bottom = (stackaddr as usize) + stacksize;
        let top = stackaddr as usize;

        StackBounds { bottom, top }
    }
}

/// Retrieve the stack bounds for the current thread (macOS implementation).
#[cfg(all(target_os = "macos", not(miri)))]
pub fn get_stack_bounds() -> StackBounds {
    use libc::{pthread_get_stackaddr_np, pthread_get_stacksize_np, pthread_self};

    unsafe {
        let stackaddr = pthread_get_stackaddr_np(pthread_self());
        let stacksize = pthread_get_stacksize_np(pthread_self());

        let bottom = stackaddr as usize;
        let top = bottom - stacksize;

        StackBounds { bottom, top }
    }
}

/// Retrieve the stack bounds for the current thread (Windows implementation).
///
/// Uses `VirtualQuery` to find the stack's allocation base. This is robust
/// but not the fastest approach. For hot paths, could use NtCurrentTeb()->StackBase
/// which is ~10x faster but requires more fragile code.
#[cfg(all(target_os = "windows", not(miri)))]
pub fn get_stack_bounds() -> StackBounds {
    use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION};

    let local_var_addr = std::ptr::addr_of!(local_var_addr) as *const u8;

    unsafe {
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        let result = VirtualQuery(local_var_addr as *const _, &mut mbi);
        assert!(result != 0, "VirtualQuery failed");

        let bottom = mbi.AllocationBase as usize;
        let top = local_var_addr as usize;

        StackBounds { bottom, top }
    }
}

/// Retrieve the stack bounds for the current thread (Stub for unsupported platforms).
#[cfg(all(
    not(target_os = "linux"),
    not(target_os = "macos"),
    not(target_os = "windows"),
    not(miri)
))]
pub fn get_stack_bounds() -> StackBounds {
    unimplemented!("Stack bounds retrieval only implemented for Linux, macOS, and Windows")
}

/// Spill CPU registers onto the stack and execute a closure to scan the stack.
///
/// This ensures all callee-saved registers are flushed to the
/// stack, allowing a conservative scan to find roots that might only exist
/// in registers.
#[inline(never)]
pub unsafe fn spill_registers_and_scan<F>(mut scan_fn: F)
where
    F: FnMut(usize, usize, bool), // val, addr, is_register
{
    // For x86_64, we spill the callee-saved registers to an array on the stack.
    // Miri does not support inline assembly, so we skip this.
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    let mut regs = [0usize; 6];
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    unsafe {
        std::arch::asm!(
            "mov {0}, rbx",
            "mov {1}, rbp",
            "mov {2}, r12",
            "mov {3}, r13",
            "mov {4}, r14",
            "mov {5}, r15",
            out(reg) regs[0],
            out(reg) regs[1],
            out(reg) regs[2],
            out(reg) regs[3],
            out(reg) regs[4],
            out(reg) regs[5],
        );
    }
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    std::hint::black_box(&regs);

    #[cfg(all(target_arch = "aarch64", not(miri)))]
    let mut regs = [0usize; 12];
    #[cfg(all(target_arch = "aarch64", not(miri)))]
    unsafe {
        std::arch::asm!(
            "mov {0}, x19",
            "mov {1}, x20",
            "mov {2}, x21",
            "mov {3}, x22",
            "mov {4}, x23",
            "mov {5}, x24",
            "mov {6}, x25",
            "mov {7}, x26",
            "mov {8}, x27",
            "mov {9}, x28",
            "mov {10}, x29",
            "mov {11}, x30",
            out(reg) regs[0],
            out(reg) regs[1],
            out(reg) regs[2],
            out(reg) regs[3],
            out(reg) regs[4],
            out(reg) regs[5],
            out(reg) regs[6],
            out(reg) regs[7],
            out(reg) regs[8],
            out(reg) regs[9],
            out(reg) regs[10],
            out(reg) regs[11],
        );
    }
    #[cfg(all(target_arch = "aarch64", not(miri)))]
    std::hint::black_box(&regs);

    // For other architectures or Miri, we might need different implementations.
    // As a fallback, we can use a large enough dummy array and black_box.
    #[cfg(any(not(target_arch = "x86_64"), not(target_arch = "aarch64"), miri))]
    let regs = [0usize; 32];
    #[cfg(any(not(target_arch = "x86_64"), not(target_arch = "aarch64"), miri))]
    std::hint::black_box(&regs);

    // Scan spilled registers explicitly as "Registers"
    for r in &regs {
        scan_fn(*r, 0, true);
    }

    let bounds = get_stack_bounds();

    // The current stack pointer is approximately the address of a local variable.
    let sp = std::ptr::addr_of!(scan_fn) as usize;

    // Scan from current SP to stack bottom.
    // We assume the stack grows downwards (high to low addresses).
    let mut current = sp & !(std::mem::align_of::<usize>() - 1);

    // println!("Scanning stack: SP={:#x}, Bottom={:#x}", sp, bounds.bottom);

    while current < bounds.bottom {
        // SAFETY: We are scanning the valid stack range of the current thread.
        // We use volatile read to avoid potential compiler optimizations,
        // though a regular read is likely fine here.
        let potential_ptr = unsafe { std::ptr::read_volatile(current as *const usize) };
        scan_fn(potential_ptr, current, false);
        current += std::mem::size_of::<usize>();
    }
}

/// Clear CPU registers to prevent "False Roots" from lingering values.
///
/// This is used by the allocator to ensure that the pointer to the newly
/// allocated page does not remain in a register (where it would be caught
/// by `spill_registers_and_scan` as a conflict).
#[inline(never)]
pub unsafe fn clear_registers() {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    unsafe {
        // Clear callee-saved registers: R12-R15
        // RBX is often reserved by LLVM, so valid pointer unlikely to be there if reserved.
        std::arch::asm!(
            // "xor rbx, rbx",
            // "xor rbp, rbp", // Don't clear RBP, it might be frame pointer!
            "xor r12, r12",
            "xor r13, r13",
            "xor r14, r14",
            "xor r15, r15",
            // out("rbx") _,
            // out("rbp") _,
            out("r12") _,
            out("r13") _,
            out("r14") _,
            out("r15") _,
        );
    }
    #[cfg(all(target_arch = "aarch64", not(miri)))]
    unsafe {
        std::arch::asm!(
            "movz x0, #0",
            "movz x1, #0",
            "movz x2, #0",
            "movz x3, #0",
            "movz x4, #0",
            "movz x5, #0",
            "movz x6, #0",
            "movz x7, #0",
            "movz x8, #0",
            "movz x9, #0",
            "movz x10, #0",
            "movz x11, #0",
            "movz x12, #0",
            "movz x13, #0",
            "movz x14, #0",
            "movz x15, #0",
            "movz x16, #0",
            "movz x17, #0",
            "movz x18, #0",
        );
    }
    // Miri/Other arch: Rely on optimization barrier or dummy work
    #[cfg(any(not(target_arch = "x86_64"), not(target_arch = "aarch64"), miri))]
    std::hint::black_box(());
}
