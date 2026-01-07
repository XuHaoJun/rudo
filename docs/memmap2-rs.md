# memmap2-rs Suitability Analysis for Address Space Coloring

## Executive Summary

Based on a code review of the `memmap2` crate (referencing the local clone at `learn-projects/memmap2-rs`), we confirm that **`memmap2` is currently unsuitable** for the specific requirements of the Allocator Squad, particularly regarding **Address Space Coloring**.

The primary limitation is the lack of API exposure for providing an address hint to the underlying OS memory mapping primitives (`mmap` on Unix, `MapViewOfFile` on Windows).

## Detailed Analysis against Requirements

### 1. Requirement: Address Hinting (0x6000_0000_0000)

**Verdict: FAILED**

The core requirement is to request a specific virtual memory address to implement "Address Space Coloring" (keeping GC objects in a high address range to avoid collisions with stack values). `memmap2` explicitly hardcodes the address argument to let the OS decide.

*   **Unix (`src/unix.rs`):**
    In `MmapInner::new`, the `mmap` system call is invoked with `ptr::null_mut()` as the first argument (`addr`). There is no option in `MmapOptions` to override this.

    ```rust
    // src/unix.rs:98-106
    unsafe {
        let ptr = mmap(
            ptr::null_mut(), // Hardcoded NULL: OS chooses address
            map_len as libc::size_t,
            prot,
            flags,
            file,
            aligned_offset as off_t,
        );
        // ...
    }
    ```

*   **Windows (`src/windows.rs`):**
    In `MmapInner::new`, the crate uses `MapViewOfFile`. This function does not accept a base address. The alternative `MapViewOfFileEx` (which does accept `lpBaseAddress`) is not used. Furthermore, for implicit anonymous mappings, it relies on `CreateFileMappingW` backed by the paging file, again without mechanisms to specify `VirtualAlloc`-style specific addresses easily through this abstraction.

    ```rust
    // src/windows.rs:188-194
    let ptr = MapViewOfFile(
        mapping,
        access,
        (aligned_offset >> 16 >> 16) as DWORD,
        (aligned_offset & 0xffffffff) as DWORD,
        aligned_len as SIZE_T,
    );
    // MapViewOfFile signature does not have a parameter for base address.
    ```

### 2. Requirement: Strictness Check & No Replacement

**Verdict: NOT SUPPORTED**

Since we cannot pass an address, we cannot enact a strict check on whether the OS honored that address.
Additionally, `memmap2` does not expose flags like `MAP_FIXED_NOREPLACE` (Linux 4.17+) which are crucial for safe fixed-address mapping without clobbering existing mappings.

### 3. Requirement: Platform Quirks (e.g. MAP_JIT)

**Verdict: PARTIALLY SUPPORTED / ABSTRACTION LEAK**

While `memmap2` handles some platform quirks (like `MAP_STACK` or `MAP_HUGETLB`), it does not seem to expose `MAP_JIT` (for Apple Silicon) in its public `MmapOptions`. Adding support for this would likely require using `Ext` traits or modifying the crate, leading to the "leaky abstraction" problem McCarthy warned about.

## Conclusion

The `memmap2` crate is designed for general-purpose file mapping and anonymous memory, prioritizing cross-platform ease of use over low-level address space control.

For the `rudo-gc` project, adhering to McCarthy's advice to implement a lightweight `sys_alloc` module using `libc` and `windows-sys` is the correct engineering decision. It provides the necessary control to implement Address Space Coloring and reduces dependencies.

## Technical Insights for `sys_alloc` Implementation

Although we cannot use `memmap2` directly, its implementation contains valuable robust patterns that we should adapt for our `sys_alloc` module.

### 1. Robust Page Size & Granularity Discovery

We should adopt the lazy-initialized, cached approach for retrieving system page sizes to avoid repeated syscalls.

**Unix (`sysconf` + Atomics):**
We should reuse this pattern for querying `_SC_PAGESIZE`.
```rust
fn page_size() -> usize {
    static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);
    match PAGE_SIZE.load(Ordering::Relaxed) {
        0 => {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
            PAGE_SIZE.store(page_size, Ordering::Relaxed);
            page_size
        }
        size => size,
    }
}
```

**Windows (Allocation Granularity vs Page Size):**
A critical detail from `src/windows.rs` is distinguishing between *Page Size* (usually 4KB) and *Allocation Granularity* (usually 64KB).
*   `VirtualAlloc` addresses must be aligned to the **allocation granularity**, not just the page size.
*   We must fetch this via `GetSystemInfo`.

```rust
fn allocation_granularity() -> usize {
    unsafe {
        let mut info = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetSystemInfo(&mut info);
        info.dwAllocationGranularity as usize
    }
}
```

### 2. Cross-Platform Flag Definitions

`memmap2` has done the heavy lifting of handling `libc` flag availability across different Unix-likes (Linux vs Android vs BSD). We should copy these conditional definitions to `sys_alloc` to ensure our GC compiles on non-standard Linux targets.

*   `MAP_STACK`: Necessary for some platforms if we use this for stack implementation (though less relevant for Heap pages).
*   `MAP_POPULATE` / `MAP_HUGETLB`: Linux specific.
*   `MAP_NORESERVE`: Useful for our "Reserve -> Commit" two-step allocation strategy (if we adopt one).

### 3. Safety & Validation Logic

*   **Size Checking:** `memmap2` validates that lengths do not exceed `isize::MAX` (implementation limit for Rust slices). We should include this check (`validate_len`) to prevent UB when converting raw pointers to `&[u8]`.
*   **Descructor Safety:** The `Drop` implementation for `MmapInner` correctly ignores return values from `munmap`, as panicking in destructors is dangerous.
    ```rust
    impl Drop for OsAllocator {
        fn drop(&mut self) {
            unsafe { libc::munmap(self.ptr, self.len) };
            // Intentionally ignore result to avoid double-panic issues
        }
    }
    ```

### 4. Zero-Length Handling (Edge Case)

`memmap2` explicitly handles `len=0` by mapping 1 byte or returning a special pointer.
*   **Decision for GC:** Our Allocator will likely deal in fixed `Page` units (e.g., 4KB, 16KB), so we can likely panic or error on `size=0` rather than supporting it, simplifying our logic compared to a general purpose crate.

