//! RAII guard for GC root registration.
//!
//! This module provides [`GcRootGuard`] for managing GC roots in async contexts.
//! [`GcRootGuard`] uses the RAII pattern to automatically unregister roots when dropped.

use std::marker::PhantomData;
use std::ptr::NonNull;

use super::root::GcRootSet;

/// A RAII guard that registers a GC root on creation and unregisters it on drop.
///
/// When a `GcRootGuard` is created, it registers the given pointer in the global
/// [`GcRootSet`]. When the guard is dropped, it automatically unregisters the
/// pointer, allowing the GC to collect the object if no other roots reference it.
///
/// # Example
///
/// ```
/// use rudo_gc::tokio::GcRootGuard;
///
/// let guard = GcRootGuard::new(/* pointer */);
/// // pointer is registered as a root
/// drop(guard);
/// // pointer is no longer a root
/// ```
///
/// # Safety
///
/// The pointer must be a valid, non-null pointer to a managed `GcBox`.
#[must_use]
pub struct GcRootGuard {
    ptr: NonNull<u8>,
    _phantom: PhantomData<u8>,
}

unsafe impl Send for GcRootGuard {}
unsafe impl Sync for GcRootGuard {}

impl GcRootGuard {
    /// Creates a new root guard for the given pointer.
    ///
    /// # Safety
    ///
    /// * `ptr` must be a valid pointer to a `GcBox<T>` for some type `T`
    /// * `ptr` must not be null
    /// * The `GcBox` must not be currently registered as a root
    pub unsafe fn new(ptr: NonNull<u8>) -> Self {
        let ptr_addr = ptr.as_ptr() as usize;
        GcRootSet::global().register(ptr_addr);

        Self {
            ptr,
            _phantom: PhantomData,
        }
    }

    /// Returns the raw pointer address of the guarded root.
    #[must_use]
    pub fn ptr(&self) -> usize {
        self.ptr.as_ptr() as usize
    }
}

impl Drop for GcRootGuard {
    fn drop(&mut self) {
        let ptr_addr = self.ptr.as_ptr() as usize;
        GcRootSet::global().unregister(ptr_addr);
    }
}
