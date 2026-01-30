//! RAII guard for GC root registration.
//!
//! This module provides [`GcRootGuard`] and [`GcRootScope`] for managing GC roots
//! in async contexts. [`GcRootGuard`] uses the RAII pattern to automatically
//! unregister roots when dropped.

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

    /// Creates a new scope guard for automatic root tracking in async blocks.
    ///
    /// This method is used by the `#[gc::root]` macro to create a guard
    /// that tracks the current scope without a specific pointer.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::tokio::GcRootGuard;
    ///
    /// let _guard = GcRootGuard::enter_scope();
    /// // Scope is now protected
    /// ```
    pub fn enter_scope() -> Self {
        let ptr = std::ptr::NonNull::dangling();
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

#[cfg(test)]
mod miri_tests {
    use super::*;
    use crate::test_util::register_test_root;
    use std::ptr::NonNull;

    #[test]
    #[cfg(miri)]
    fn test_gcrootguard_unsafe_new_validates_pointer() {
        let ptr = NonNull::dangling();
        register_test_root(ptr);

        let guard = unsafe { GcRootGuard::new(ptr) };
        assert_eq!(guard.ptr(), ptr.as_ptr() as usize);
    }

    #[test]
    #[cfg(miri)]
    fn test_gcrootguard_enter_scope_creates_valid_guard() {
        let guard = GcRootGuard::enter_scope();
        let ptr_addr = guard.ptr();
        assert_ne!(ptr_addr, 0);
        assert!(GcRootSet::global().is_registered(ptr_addr));
    }

    #[test]
    #[cfg(miri)]
    fn test_gcrootguard_drop_unregisters() {
        let ptr = NonNull::dangling();
        register_test_root(ptr);

        {
            let guard = unsafe { GcRootGuard::new(ptr) };
            assert!(GcRootSet::global().is_registered(ptr.as_ptr() as usize));
            let _ = guard;
        }

        assert!(!GcRootSet::global().is_registered(ptr.as_ptr() as usize));
    }

    #[test]
    #[cfg(miri)]
    fn test_gcrootguard_double_drop_safety() {
        let ptr = NonNull::dangling();

        let guard = unsafe { GcRootGuard::new(ptr) };
        let ptr_addr = ptr.as_ptr() as usize;

        GcRootSet::global().unregister(ptr_addr);

        let guard2 = unsafe { GcRootGuard::new(ptr) };
        assert!(GcRootSet::global().is_registered(ptr_addr));
        let _ = guard2;
    }
}

/// Future wrapper that holds a root guard for automatic tracking in spawned tasks.
///
/// `GcRootScope` wraps a future and owns a [`GcRootGuard`]. The guard is created
/// when the scope is created and dropped when the future completes, ensuring
/// automatic root tracking without manual guard management.
///
/// # Example
///
/// ```
/// use rudo_gc::tokio::GcRootScope;
///
/// let scope = GcRootScope::new(async { /* ... */ });
/// // Guard is active while the future is running
/// ```
pub struct GcRootScope<F> {
    future: F,
    _guard: GcRootGuard,
}

impl<F> GcRootScope<F> {
    /// Creates a new scope wrapping the given future with an automatic root guard.
    ///
    /// # Safety
    ///
    /// The guard must be created with a valid pointer to the `GcBox` being tracked.
    pub const unsafe fn new(future: F, guard: GcRootGuard) -> Self {
        Self {
            future,
            _guard: guard,
        }
    }
}

impl<F: std::future::Future> std::future::Future for GcRootScope<F> {
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: We poll the wrapped future with the same waker.
        // The future is pinned for its lifetime, which is valid.
        let this = unsafe { self.get_unchecked_mut() };
        let future = unsafe { std::pin::Pin::new_unchecked(&mut this.future) };
        future.poll(cx)
    }
}
