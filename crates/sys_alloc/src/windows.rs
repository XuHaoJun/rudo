use std::io::{self, Error};
use std::mem;
use std::ptr;

use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

/// Returns the system allocation granularity.
///
/// On Windows, `VirtualAlloc` address must be aligned to this value (typically 64KB),
/// which is often larger than the page size (typically 4KB).
pub fn allocation_granularity() -> usize {
    unsafe {
        let mut info: SYSTEM_INFO = mem::zeroed();
        GetSystemInfo(&mut info);
        let gran = info.dwAllocationGranularity as usize;
        if gran == 0 {
            // Fallback for Miri or environments where GetSystemInfo fails
            65536
        } else {
            gran
        }
    }
}

pub fn page_size() -> usize {
    unsafe {
        let mut info: SYSTEM_INFO = mem::zeroed();
        GetSystemInfo(&mut info);
        let size = info.dwPageSize as usize;
        if size == 0 {
            // Fallback for Miri or environments where GetSystemInfo fails
            4096
        } else {
            size
        }
    }
}

pub struct MmapInner {
    ptr: *mut std::ffi::c_void,
    len: usize,
}

impl MmapInner {
    /// Creates a new anonymous memory mapping with an optional address hint.
    pub unsafe fn map_anon(
        hint_addr: usize,
        len: usize,
        _populate: bool,
        _no_reserve: bool,
    ) -> io::Result<MmapInner> {
        let addr = if hint_addr == 0 {
            ptr::null()
        } else {
            hint_addr as *const std::ffi::c_void
        };

        // Windows requires MEM_RESERVE | MEM_COMMIT to actually get usable memory
        let mut ptr = unsafe { VirtualAlloc(addr, len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };

        // If strict allocation at hint_addr failed, and we had a hint, try letting the OS decide.
        if ptr.is_null() && !addr.is_null() {
            ptr =
                unsafe { VirtualAlloc(ptr::null(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
        }

        if ptr.is_null() {
            return Err(Error::last_os_error());
        }

        Ok(MmapInner { ptr, len })
    }

    pub const fn ptr(&self) -> *mut u8 {
        self.ptr.cast::<u8>()
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    /// Creates a `MmapInner` from a raw pointer and length.
    pub const unsafe fn from_raw(ptr: *mut u8, len: usize) -> Self {
        Self {
            ptr: ptr.cast::<std::ffi::c_void>(),
            len,
        }
    }
}

impl Drop for MmapInner {
    fn drop(&mut self) {
        if self.len > 0 {
            unsafe {
                // MEM_RELEASE requires dwSize to be 0
                VirtualFree(self.ptr, 0, MEM_RELEASE);
            }
        }
    }
}

unsafe impl Send for MmapInner {}
unsafe impl Sync for MmapInner {}
