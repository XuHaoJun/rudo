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

/// Retrieve the stack bounds for the current thread (Stub for non-Linux).
#[cfg(all(not(target_os = "linux"), not(miri)))]
pub fn get_stack_bounds() -> StackBounds {
    unimplemented!("Stack bounds retrieval only implemented for Linux")
}

/// Spill CPU registers onto the stack and execute a closure to scan the stack.
///
/// This ensures all callee-saved registers are flushed to the
/// stack, allowing a conservative scan to find roots that might only exist
/// in registers.
#[inline(never)]
pub unsafe fn spill_registers_and_scan<F>(mut scan_fn: F)
where
    F: FnMut(usize),
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

    // For other architectures or Miri, we might need different implementations.
    // As a fallback, we can use a large enough dummy array and black_box.
    #[cfg(any(not(target_arch = "x86_64"), miri))]
    let regs = [0usize; 32];
    #[cfg(any(not(target_arch = "x86_64"), miri))]
    std::hint::black_box(&regs);

    let bounds = get_stack_bounds();

    // The current stack pointer is approximately the address of a local variable.
    let sp = std::ptr::addr_of!(scan_fn) as usize;

    // Scan from current SP to stack bottom.
    // We assume the stack grows downwards (high to low addresses).
    let mut current = sp & !(std::mem::align_of::<usize>() - 1);

    // eprintln!("Scanning stack: SP={:#x}, Bottom={:#x}", sp, bounds.bottom);

    while current < bounds.bottom {
        // SAFETY: We are scanning the valid stack range of the current thread.
        // We use volatile read to avoid potential compiler optimizations,
        // though a regular read is likely fine here.
        let potential_ptr = unsafe { std::ptr::read_volatile(current as *const usize) };
        scan_fn(potential_ptr);
        current += std::mem::size_of::<usize>();
    }
}
