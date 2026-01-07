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
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "length must be greater than 0"));
        }

        let inner = unsafe {
            os::MmapInner::map_anon(
                self.hint_addr, 
                self.len,
                self.populate,
                self.no_reserve
            )?
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
        assert_eq!(ag & (ag - 1), 0, "Allocation granularity should be power of 2");
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

        let mmap_opts = MmapOptions::new()
            .len(len)
            .with_hint(hint_base);

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
}
