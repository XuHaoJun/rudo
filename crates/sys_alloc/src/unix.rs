use std::io::{self, Error};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(any(target_os = "linux", target_os = "android"))]
const MAP_POPULATE: libc::c_int = libc::MAP_POPULATE;

#[cfg(not(any(target_os = "linux", target_os = "android")))]
const MAP_POPULATE: libc::c_int = 0;

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    target_os = "netbsd",
    target_os = "solaris",
    target_os = "illumos",
))]
const MAP_NORESERVE: libc::c_int = libc::MAP_NORESERVE;

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    target_os = "netbsd",
    target_os = "solaris",
    target_os = "illumos",
)))]
const MAP_NORESERVE: libc::c_int = 0;

/// Returns the system page size, cached atomically.
pub fn page_size() -> usize {
    static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

    match PAGE_SIZE.load(Ordering::Relaxed) {
        0 => {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
            PAGE_SIZE.store(page_size, Ordering::Relaxed);
            page_size
        }
        page_size => page_size,
    }
}

pub struct MmapInner {
    ptr: *mut libc::c_void,
    len: usize,
}

impl MmapInner {
    /// Creates a new anonymous memory mapping with an optional address hint.
    /// 
    /// # Safety
    /// 
    /// This function is unsafe because it calls `mmap`.
    pub unsafe fn map_anon(
        hint_addr: usize,
        len: usize,
        populate: bool,
        no_reserve: bool,
    ) -> io::Result<MmapInner> {
        let populate = if populate { MAP_POPULATE } else { 0 };
        let no_reserve = if no_reserve { MAP_NORESERVE } else { 0 };
        
        let addr = if hint_addr == 0 {
            ptr::null_mut()
        } else {
            hint_addr as *mut libc::c_void
        };

        // Standard flags for anonymous mapping
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON | populate | no_reserve;
        let prot = libc::PROT_READ | libc::PROT_WRITE;

        let ptr = unsafe {
            libc::mmap(
                addr,
                len,
                prot,
                flags,
                -1,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err(Error::last_os_error());
        }

        // Basic verification: if we gave a hint, did we get it?
        // Note: we don't enforce strictness here (returning error if mismatch), 
        // that is up to the higher level policy. 
        // But for Address Space Coloring, the caller needs to check `ptr`.

        Ok(MmapInner { ptr, len })
    }

    pub fn ptr(&self) -> *mut u8 {
        self.ptr as *mut u8
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl Drop for MmapInner {
    fn drop(&mut self) {
        if self.len > 0 {
            unsafe {
                libc::munmap(self.ptr, self.len);
            }
        }
    }
}

unsafe impl Send for MmapInner {}
unsafe impl Sync for MmapInner {}
