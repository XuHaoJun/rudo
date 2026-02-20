//! Cross-thread GC handle implementation.
//!
//! This module provides `GcHandle<T>` and `WeakCrossThreadHandle<T>` types
//! that allow safe hand-off of GC-managed object references between threads.
//!
//! # Overview
//!
//! Cross-thread handles are `Send + Sync` even when `T` is not, enabling
//! frameworks to schedule UI updates from async threads without requiring
//! signal types to implement thread-safe traits.
//!
//! # Safety
//!
//! The core safety argument for `GcHandle<T>` being `Send + Sync` even when
//! `T: !Send`:
//!
//! 1. **No direct access to `T` from non-origin threads.** The handle is an
//!    opaque token — it stores no reference through which `T` can be read or
//!    written. The only way to obtain a `Gc<T>` (and thus access `T`) is via
//!    `resolve()`, which enforces origin-thread affinity at runtime.
//!
//! 2. **Origin-thread enforcement is a hard check, not advisory.** `resolve()`
//!    compares `std::thread::current().id()` against the stored `origin_thread`.
//!    This is a panic, not UB — the invariant is enforced before any access
//!    to `T`.
//!
//! 3. **Root registration keeps the object alive.** The handle holds an
//!    `Arc<ThreadControlBlock>` and the root entry is stored in the TCB's
//!    `Mutex`-protected handle list.

use std::ptr::NonNull;
use std::sync::Arc;
use std::thread::ThreadId;

use crate::heap::{HandleId, ThreadControlBlock};
use crate::ptr::GcBox;
use crate::ptr::GcBoxWeakRef;
use crate::trace::Trace;
use crate::Gc;

/// Strong cross-thread handle — keeps the referenced object alive.
///
/// Created via `Gc::cross_thread_handle()`. The handle is `Send + Sync`
/// regardless of whether `T` is, because it never exposes `T` directly.
/// Resolution back to `Gc<T>` is only permitted on the origin thread.
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
///
/// #[derive(Trace)]
/// struct SignalData {
///     value: i32,
/// }
///
/// // Create a GC object on the main thread
/// let gc: Gc<SignalData> = Gc::new(SignalData { value: 42 });
///
/// // Create a cross-thread handle
/// let handle = gc.cross_thread_handle();
///
/// // The handle can be sent to any thread
/// // but can only be resolved on the origin thread
/// ```
pub struct GcHandle<T: Trace + 'static> {
    /// Raw pointer to the `GcBox`. Validity is guaranteed by root registration.
    pub(crate) ptr: NonNull<GcBox<T>>,
    /// TCB of the origin thread. Prevents TCB deallocation; holds root list.
    pub(crate) origin_tcb: Arc<ThreadControlBlock>,
    /// Origin thread identity, for resolve-time check.
    pub(crate) origin_thread: ThreadId,
    /// Unique ID for this handle's root entry (for O(1) unregistration).
    pub(crate) handle_id: HandleId,
}

unsafe impl<T: Trace + 'static> Send for GcHandle<T> {}

unsafe impl<T: Trace + 'static> Sync for GcHandle<T> {}

impl<T: Trace + 'static> GcHandle<T> {
    /// Returns the thread where this handle was created.
    #[must_use]
    pub fn origin_thread(&self) -> ThreadId {
        self.origin_thread
    }

    /// Returns `true` if the underlying object is still alive.
    ///
    /// For strong handles this is `true` while the handle is registered.
    /// Returns `false` if the handle was unregistered or if the origin thread's
    /// heap was torn down (in which case [`resolve()`] would panic).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.handle_id != HandleId::INVALID
    }

    /// Explicitly unregisters this handle from the root set.
    ///
    /// After unregistration, `resolve()` will panic and `is_valid()` returns `false`.
    /// The object becomes eligible for collection (unless other roots exist).
    ///
    /// This is idempotent — calling it multiple times is safe.
    pub fn unregister(&mut self) {
        if self.handle_id == HandleId::INVALID {
            return;
        }
        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
        self.handle_id = HandleId::INVALID;
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
    }

    /// Resolves this handle to a `Gc<T>` on the origin thread.
    ///
    /// # Panics
    ///
    /// Panics if called from a thread other than the origin thread. This includes
    /// the case where the origin thread has already terminated, since the current
    /// thread can never match a terminated thread's ID.
    ///
    /// When thread affinity is uncertain (e.g., callbacks that may run on different
    /// threads), use [`try_resolve()`] instead to get `None` instead of panicking.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let gc: Gc<Data> = Gc::new(Data { value: 42 });
    /// let handle = gc.cross_thread_handle();
    ///
    /// // On the origin thread, resolve to access the data
    /// let resolved: Gc<Data> = handle.resolve();
    /// assert_eq!(resolved.value, 42);
    /// ```
    #[track_caller]
    pub fn resolve(&self) -> Gc<T> {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "GcHandle::resolve() must be called on the origin thread \
             (origin={:?}, current={:?})",
            self.origin_thread,
            std::thread::current().id(),
        );
        // Take ownership of one ref for the returned Gc. The handle holds refs;
        // resolving transfers one to the caller. SAFETY: verified on origin thread.
        unsafe {
            (*self.ptr.as_ptr()).inc_ref();
            Gc::from_raw(self.ptr.as_ptr() as *const u8)
        }
    }

    /// Tries to resolve, returning `None` if called from the wrong thread.
    ///
    /// Returns `None` when:
    /// - Called from a thread other than the origin thread, or
    /// - The origin thread has already terminated.
    ///
    /// Use this instead of [`resolve()`] when thread affinity is uncertain.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let gc: Gc<Data> = Gc::new(Data { value: 42 });
    /// let handle = gc.cross_thread_handle();
    ///
    /// // Try to resolve - returns None if on wrong thread
    /// if let Some(resolved) = handle.try_resolve() {
    ///     // Safe to use resolved
    /// }
    /// ```
    #[must_use]
    pub fn try_resolve(&self) -> Option<Gc<T>> {
        if std::thread::current().id() != self.origin_thread {
            return None;
        }
        // Take ownership of one ref for the returned Gc. SAFETY: same as resolve().
        unsafe {
            (*self.ptr.as_ptr()).inc_ref();
            Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
        }
    }

    /// Downgrades to a weak cross-thread handle.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let gc: Gc<Data> = Gc::new(Data { value: 42 });
    /// let handle = gc.cross_thread_handle();
    ///
    /// let weak = handle.downgrade();
    /// // weak doesn't keep the object alive
    /// ```
    #[must_use]
    pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
        unsafe {
            (*self.ptr.as_ptr()).inc_weak();
        }
        WeakCrossThreadHandle {
            weak: GcBoxWeakRef::new(self.ptr),
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        }
    }
}

impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        assert_ne!(
            self.handle_id,
            HandleId::INVALID,
            "cannot clone an unregistered GcHandle"
        );

        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        let new_id = roots.allocate_id();
        roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
        unsafe { (*self.ptr.as_ptr()).inc_ref() };
        drop(roots);

        Self {
            ptr: self.ptr,
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
            handle_id: new_id,
        }
    }
}

impl<T: Trace + 'static> Drop for GcHandle<T> {
    /// Unregisters the root entry and releases the reference count held by this handle.
    ///
    /// Safe to call from any thread: the TCB is held via `Arc`, and the root
    /// list is `Mutex`-protected. No thread-local storage is accessed.
    fn drop(&mut self) {
        if self.handle_id == HandleId::INVALID {
            return;
        }
        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
        self.handle_id = HandleId::INVALID;
        // Release the ref count we held. May trigger object drop if this was last ref.
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
    }
}

impl<T: Trace + 'static> std::fmt::Debug for GcHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcHandle")
            .field("origin_thread", &self.origin_thread)
            .field("handle_id", &self.handle_id)
            .field("is_valid", &self.is_valid())
            .finish_non_exhaustive()
    }
}

/// Weak cross-thread handle — does not prevent collection.
///
/// Created via `Gc::weak_cross_thread_handle()` or `GcHandle::downgrade()`.
/// Like `GcHandle`, the handle is `Send + Sync` but resolve is origin-thread-only
/// (because `T` may be `!Send`).
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
///
/// #[derive(Trace)]
/// struct Data { value: i32 }
///
/// let gc: Gc<Data> = Gc::new(Data { value: 42 });
/// let weak = gc.weak_cross_thread_handle();
///
/// // weak doesn't keep gc alive - if gc is dropped, resolve returns None
/// ```
pub struct WeakCrossThreadHandle<T: Trace + 'static> {
    pub(crate) weak: GcBoxWeakRef<T>,
    pub(crate) origin_tcb: Arc<ThreadControlBlock>,
    pub(crate) origin_thread: ThreadId,
}

unsafe impl<T: Trace + 'static> Send for WeakCrossThreadHandle<T> {}

unsafe impl<T: Trace + 'static> Sync for WeakCrossThreadHandle<T> {}

impl<T: Trace + 'static> WeakCrossThreadHandle<T> {
    /// Returns the thread where this handle was created.
    #[must_use]
    pub fn origin_thread(&self) -> ThreadId {
        self.origin_thread
    }

    /// Returns `true` if `upgrade()` would succeed.
    ///
    /// This checks whether the underlying object is still alive and not being
    /// dropped. Note that even if `is_valid()` returns `true`, another thread
    /// may collect the object immediately after this call returns.
    /// Use `upgrade()` (which atomically transitions `ref_count`) to safely
    /// obtain a strong reference.
    ///
    /// Can be called from any thread (doesn't access `T`).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.weak.upgrade().is_some()
    }

    /// Resolves to a `Gc<T>` if the object is still alive.
    ///
    /// # Safety
    ///
    /// Must be called from the origin thread. `T` may be `!Send`.
    ///
    /// # Panics
    ///
    /// Panics if called from a thread other than the origin thread,
    /// because `T` may be `!Send`.
    #[track_caller]
    pub fn resolve(&self) -> Option<Gc<T>> {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "WeakCrossThreadHandle::resolve() must be called on the origin thread"
        );
        // Weak handle does not prevent collection. Check liveness first.
        self.weak.upgrade()
    }

    /// Tries to resolve, returning `None` if called from wrong thread
    /// or if the object has been collected.
    #[must_use]
    pub fn try_resolve(&self) -> Option<Gc<T>> {
        if std::thread::current().id() != self.origin_thread {
            return None;
        }
        self.weak.upgrade()
    }

    /// Check if this weak reference might be valid.
    ///
    /// This is a lightweight check that doesn't require dereferencing.
    /// Returns `false` if the weak ref is definitely invalid.
    /// Returns `true` if it might be valid (needs `try_upgrade` to confirm).
    #[inline]
    #[must_use]
    pub fn may_be_valid(&self) -> bool {
        self.weak.may_be_valid()
    }

    /// Attempt to upgrade with additional safety checks.
    ///
    /// Returns `None` if:
    /// - The weak ref is null
    /// - The object has been collected
    /// - The memory location is obviously invalid (misaligned or too low address)
    ///
    /// # Safety
    ///
    /// Must be called from the origin thread. `T` may be `!Send`.
    ///
    /// # Panics
    ///
    /// Panics if called from a thread other than the origin thread,
    /// because `T` may be `!Send`.
    #[track_caller]
    pub fn try_upgrade(&self) -> Option<Gc<T>> {
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "WeakCrossThreadHandle::try_upgrade() must be called on the origin thread"
        );
        self.weak.try_upgrade()
    }
}

impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    fn clone(&self) -> Self {
        Self {
            weak: self.weak.clone(),
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        }
    }
}

impl<T: Trace + 'static> Drop for WeakCrossThreadHandle<T> {
    fn drop(&mut self) {
        let ptr = self.weak.as_ptr();
        let Some(ptr) = ptr else {
            return;
        };
        unsafe {
            (*ptr.as_ptr()).dec_weak();
        }
    }
}

impl<T: Trace + 'static> std::fmt::Debug for WeakCrossThreadHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakCrossThreadHandle")
            .field("origin_thread", &self.origin_thread)
            .field("is_valid", &self.is_valid())
            .finish_non_exhaustive()
    }
}
