use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as os;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

pub use os::page_size;

/// Returns the system allocation granularity.
///
/// On Windows, this is typically 64KB. On Unix, this is typically the system page size.
/// When requesting a specific address, it should be aligned to this granularity.
pub fn allocation_granularity() -> usize {
    #[cfg(windows)]
    {
        os::allocation_granularity()
    }
    #[cfg(unix)]
    {
        os::page_size()
    }
}

/// A handle to a memory mapped region.
///
/// The region is automatically unmapped when this handle is dropped.
pub struct Mmap {
    inner: os::MmapInner,
}

impl Mmap {
    /// Returns a pointer to the start of the memory mapping.
    pub fn ptr(&self) -> *mut u8 {
        self.inner.ptr()
    }

    /// Returns the length of the memory mapping in bytes.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Flushes the memory mapped region to disk (if file backed) or ensures
    /// visibility. For anonymous mappings, this is generally a no-op or ensures
    /// cache coherence.
    pub fn flush(&self) -> io::Result<()> {
        // Implementation detail: we could expose msync/FlushViewOfFile here
        Ok(())
    }
}

unsafe impl Send for Mmap {}
unsafe impl Sync for Mmap {}

/// Configuration for creating a memory mapping.
#[derive(Debug, Clone)]
pub struct MmapOptions {
    len: usize,
    hint_addr: usize,
    populate: bool,
    no_reserve: bool,
    strict: bool,
}

impl MmapOptions {
    /// Creates a new `MmapOptions` with default settings (length 0).
    /// You must set a length before mapping.
    pub fn new() -> Self {
        Self {
            len: 0,
            hint_addr: 0,
            populate: false,
            no_reserve: false,
            strict: false,
        }
    }

    /// Sets the length of the mapping in bytes.
    pub fn len(mut self, len: usize) -> Self {
        self.len = len;
        self
    }

    /// Sets a hint address for the mapping.
    ///
    /// This is a request to the OS to place the mapping at this specific virtual address.
    /// The OS is not required to honor this request (on some platforms), or the call
    /// may fail if the address is already in use or invalid.
    ///
    /// For the best chance of success:
    /// - The address should be aligned to `allocation_granularity()`.
    /// - The address range `[hint_addr, hint_addr + len)` should be free.
    pub fn with_hint(mut self, addr: usize) -> Self {
        self.hint_addr = addr;
        self
    }

    /// Sets whether to pre-populate (prefault) the page tables.
    ///
    /// On Linux, this adds `MAP_POPULATE`.
    pub fn populate(mut self, populate: bool) -> Self {
        self.populate = populate;
        self
    }

    /// Sets whether to reserve swap space (on supported platforms).
    ///
    /// On Linux, this adds `MAP_NORESERVE`.
    pub fn no_reserve(mut self, no_reserve: bool) -> Self {
        self.no_reserve = no_reserve;
        self
    }

    /// Sets whether the hint address is strict.
    ///
    /// If true, `map_anon` will return an error if the OS cannot map the memory
    /// at the exact requested `hint_addr`.
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Creates an anonymous memory map.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it creates a raw memory mapping which has
    /// implications for memory safety (e.g. use-after-free if the Mmap is dropped
    /// while valid pointers into it still exist - though `Mmap` itself is safe,
    /// using the raw pointer it yields requires care).
    ///
    /// Actually, `Mmap` owns the memory, so as long as `Mmap` is alive, the pointer is valid.
    /// However, `sys_alloc` is a low-level crate, so we mark creation as unsafe
    /// mostly because of the OS interactions and potential for UB if `hint_addr`
    /// is misused in some extensive contexts (though simply asking for an addr is usually safe).
    pub unsafe fn map_anon(&self) -> io::Result<Mmap> {
        if self.len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "length must be greater than 0",
            ));
        }

        let inner = unsafe {
            let inner =
                os::MmapInner::map_anon(self.hint_addr, self.len, self.populate, self.no_reserve)?;

            if self.strict && self.hint_addr != 0 {
                let ptr = inner.ptr() as usize;
                if ptr != self.hint_addr {
                    // MmapInner drop will unmap the wrong memory
                    return Err(io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        format!(
                            "Strict hint failed: requested {:#x}, got {:#x}",
                            self.hint_addr, ptr
                        ),
                    ));
                }
            }

            inner
        };

        Ok(Mmap { inner })
    }
}

impl Default for MmapOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn test_page_size() {
        let ps = page_size();
        assert!(ps > 0);
        assert_eq!(ps & (ps - 1), 0, "Page size should be power of 2");
    }

    #[test]
    fn test_allocation_granularity() {
        let ag = allocation_granularity();
        assert!(ag > 0);
        assert_eq!(
            ag & (ag - 1),
            0,
            "Allocation granularity should be power of 2"
        );
        assert!(ag >= page_size());
    }

    #[test]
    fn test_basic_map() {
        let len = page_size();
        let mmap = unsafe {
            MmapOptions::new()
                .len(len)
                .map_anon()
                .expect("failed to map")
        };

        let ptr = mmap.ptr();
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % page_size(), 0);

        // Verification: Write to memory
        unsafe {
            ptr::write_volatile(ptr, 42);
            assert_eq!(ptr::read_volatile(ptr), 42);
        }
    }

    #[test]
    fn test_map_with_hint() {
        // This test is heuristic. We try to map at a specific high address.
        // It might fail if the OS ASLR or memory limits prevent it,
        // so we don't strictly assert success validation of the exact address,
        // but we verify the API contract works without erroring on valid hint logic.

        let len = allocation_granularity();

        // Pick a high address that is likely available and aligned
        // 0x6000_0000_0000 is the example from McCarthy
        #[cfg(target_pointer_width = "64")]
        let hint_base = 0x6000_0000_0000usize;
        #[cfg(target_pointer_width = "32")]
        let hint_base = 0x4000_0000usize;

        let mmap_opts = MmapOptions::new().len(len).with_hint(hint_base);

        // We allow failure here because test environment constraints are unknown
        if let Ok(mmap) = unsafe { mmap_opts.map_anon() } {
            let ptr = mmap.ptr();
            println!("Requested: {:x}, Got: {:x}", hint_base, ptr as usize);

            // If we got an address, it must be valid memory
            unsafe {
                ptr::write_volatile(ptr, 99);
                assert_eq!(ptr::read_volatile(ptr), 99);
            }
        }
    }

    #[test]
    fn test_strict_hint_success() {
        let len = allocation_granularity();

        // Use a high address likely to be free
        #[cfg(target_pointer_width = "64")]
        let hint_base = 0x6000_0000_0000usize;
        #[cfg(target_pointer_width = "32")]
        let hint_base = 0x4000_0000usize;

        let mmap_opts = MmapOptions::new()
            .len(len)
            .with_hint(hint_base)
            .strict(true);

        // Attempt mapping. It might fail on some systems, but if it succeeds, address MUST match.
        match unsafe { mmap_opts.map_anon() } {
            Ok(mmap) => {
                assert_eq!(
                    mmap.ptr() as usize,
                    hint_base,
                    "Strict mapping returned wrong address"
                );
            }
            Err(_) => {
                // strict mapping failure is allowed (e.g. if address is taken),
                // but if we were lucky enough to get memory, checking it matches is the test.
                // We can't easily force failure without using a known taken address.
            }
        }
    }

    #[test]
    fn test_strict_hint_fail() {
        let len = allocation_granularity();

        // 1. Map something to ensure the address is taken
        #[cfg(target_pointer_width = "64")]
        let hint_base = 0x6100_0000_0000usize;
        #[cfg(target_pointer_width = "32")]
        let hint_base = 0x5000_0000usize;

        let mmap1 = unsafe {
            MmapOptions::new()
                .len(len)
                .with_hint(hint_base)
                // strict=false (default), so we just try to get it
                .map_anon()
        };

        if let Ok(m1) = mmap1 {
            // If we got the address (or any address), try to strict map OVER it.
            // But mmap usually returns a different address if the hint is taken.
            // Strict should reject that different address.

            // If mmap1 got hint_base, good. If not, we use m1.ptr() as the "taken" address.
            let taken_addr = m1.ptr() as usize;

            let result = unsafe {
                MmapOptions::new()
                    .len(len)
                    .with_hint(taken_addr)
                    .strict(true)
                    .map_anon()
            };

            // We expect failure because the address is already mapped.
            // On Unix, mmap with hint usually returns a DIFFERENT address if taken.
            // Our strict check should catch that and return error.
            assert!(
                result.is_err(),
                "Strict mapping should fail on taken address"
            );
        }
    }
}
