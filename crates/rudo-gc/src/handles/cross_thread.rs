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
    /// For strong handles this is always `true` while the handle is registered,
    /// unless the origin thread's heap has been torn down.
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
        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
        self.handle_id = HandleId::INVALID;
    }

    /// Resolves this handle to a `Gc<T>` on the origin thread.
    ///
    /// # Panics
    ///
    /// Panics if called from a thread other than the origin thread.
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
        // SAFETY: The root registration guarantees the object is alive.
        // We've verified we're on the origin thread, so producing a Gc<T>
        // is safe even if T: !Send.
        unsafe { Gc::from_raw(self.ptr.as_ptr() as *const u8) }
    }

    /// Tries to resolve, returning `None` if called from wrong thread.
    ///
    /// Useful in contexts where you cannot guarantee which thread you're on.
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
        // SAFETY: same as resolve().
        Some(unsafe { Gc::from_raw(self.ptr.as_ptr() as *const u8) })
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
    /// Unregisters the root entry from the origin thread's TCB.
    ///
    /// Safe to call from any thread: the TCB is held via `Arc`, and the root
    /// list is `Mutex`-protected. No thread-local storage is accessed.
    fn drop(&mut self) {
        // Lock the origin thread's root table. This is safe from any thread
        // because origin_tcb is an Arc<ThreadControlBlock>.
        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        // Lock released here. The object becomes eligible for collection
        // on the next GC cycle (unless other roots exist).
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

    /// Returns `true` if the underlying object is still alive.
    ///
    /// Can be called from any thread (doesn't access `T`).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.weak.upgrade().is_some()
    }

    /// Resolves to a `Gc<T>` if the object is still alive.
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

impl<T: Trace + 'static> std::fmt::Debug for WeakCrossThreadHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakCrossThreadHandle")
            .field("origin_thread", &self.origin_thread)
            .field("is_valid", &self.is_valid())
            .finish_non_exhaustive()
    }
}
