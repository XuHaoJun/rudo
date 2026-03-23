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
//!    first verifies the origin TCB is still alive (the `Weak<ThreadControlBlock>`
//!    upgrade is the authoritative liveness check — `ThreadId`s can be reused after
//!    termination), then compares `std::thread::current().id()`. This is a panic,
//!    not UB — the invariant is enforced before any access to `T`.
//!
//! 3. **Root registration keeps the object alive.** The handle holds an
//!    `Arc<ThreadControlBlock>` and the root entry is stored in the TCB's
//!    `Mutex`-protected handle list.

use std::ptr::NonNull;
use std::sync::{Arc, Weak};
use std::thread::ThreadId;

use crate::heap::{self, HandleId, ThreadControlBlock};
use crate::ptr::is_gc_box_pointer_valid;
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
    /// TCB of the origin thread. Weak allows TCB to be dropped when thread terminates.
    pub(crate) origin_tcb: Weak<ThreadControlBlock>,
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
    /// Returns `false` if the handle was unregistered. Note: when the origin
    /// thread has terminated, [`resolve()`] will panic (use [`try_resolve()`]).
    ///
    /// Checks both `handle_id` and root list presence to match [`resolve()`]
    /// semantics and avoid TOCTOU where another thread unregisters between
    /// `is_valid()` and `resolve()`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.handle_id == HandleId::INVALID {
            return false;
        }
        // TCB-first + orphan fallback (bug313): checking orphan then releasing the lock
        // allowed a race with `migrate_roots_to_orphan` — another thread could see an empty
        // TCB root map while the entry already lives only in the orphan table.
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let roots = tcb.cross_thread_roots.lock().unwrap();
            if roots.strong.contains_key(&self.handle_id) {
                return true;
            }
        }
        let orphan = heap::lock_orphan_roots();
        orphan.contains_key(&(self.origin_thread, self.handle_id))
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
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.remove(&self.handle_id);
            drop(roots);
        } else {
            let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
        }
        self.handle_id = HandleId::INVALID;
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
    }

    /// Resolves this handle to a `Gc<T>` on the origin thread.
    ///
    /// # Panics
    ///
    /// Panics if called from a thread other than the origin thread (different
    /// `ThreadId`). When the origin thread has terminated, roots are migrated to
    /// the orphan table; resolution can still succeed from a thread with the same
    /// `ThreadId` (reuse), preserving orphan handle resolution.
    ///
    /// **When the origin thread may have terminated** and you might be on a
    /// different thread, use [`try_resolve()`] instead to get `None` without panicking.
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
    #[allow(clippy::significant_drop_tightening)] // Lock held through inc_ref
    pub fn resolve(&self) -> Gc<T> {
        assert!(
            self.handle_id != HandleId::INVALID,
            "GcHandle::resolve: handle has been unregistered"
        );
        // Require origin thread (ThreadId match) before any resolution.
        // When TCB is alive, this is the actual origin thread.
        // When TCB is dead (orphan), roots are in orphan table; resolution is allowed
        // only from a thread with the same ThreadId (reuse), preserving !Send safety.
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "GcHandle::resolve() must be called on the origin thread (origin={:?}, current={:?}). \
             If the origin thread has terminated, use try_resolve() instead to get None.",
            self.origin_thread,
            std::thread::current().id(),
        );
        // TCB alive: use TCB roots. TCB dead: use orphan roots (same as downgrade, clone).
        // Hold lock during check+inc_ref to prevent TOCTOU with unregister.
        self.origin_tcb.upgrade().map_or_else(
            || {
                let orphan = heap::lock_orphan_roots();
                if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
                    panic!("GcHandle::resolve: handle has been unregistered");
                }
                self.resolve_impl()
            },
            |tcb| {
                let roots = tcb.cross_thread_roots.lock().unwrap();
                if roots.strong.contains_key(&self.handle_id) {
                    return self.resolve_impl();
                }
                drop(roots);
                let orphan = heap::lock_orphan_roots();
                if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
                    return self.resolve_impl();
                }
                drop(orphan);
                let roots = tcb.cross_thread_roots.lock().unwrap();
                if roots.strong.contains_key(&self.handle_id) {
                    return self.resolve_impl();
                }
                if self.origin_tcb.upgrade().is_some() {
                    let orphan = heap::lock_orphan_roots();
                    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
                        return self.resolve_impl();
                    }
                }
                panic!("GcHandle::resolve: handle has been unregistered");
            },
        )
    }

    /// Shared `resolve` logic. Caller must hold TCB roots lock or orphan roots lock.
    #[inline]
    #[allow(clippy::significant_drop_tightening)]
    fn resolve_impl(&self) -> Gc<T> {
        unsafe {
            // FIX bug382: Check is_allocated BEFORE dereferencing to avoid TOCTOU.
            // If slot is swept and reused between dereference and check, we'd read
            // fields from the wrong object (type confusion).
            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                assert!(
                    (*header.as_ptr()).is_allocated(idx),
                    "GcHandle::resolve: object slot was swept before dereference"
                );
            }

            let gc_box = &*self.ptr.as_ptr();
            assert!(
                !gc_box.is_under_construction(),
                "GcHandle::resolve: object is under construction"
            );
            assert!(
                !gc_box.has_dead_flag(),
                "GcHandle::resolve: object has been dropped (dead flag set)"
            );
            assert!(
                gc_box.dropping_state() == 0,
                "GcHandle::resolve: object is being dropped"
            );

            // Check is_allocated BEFORE inc_ref to avoid TOCTOU (bug345).
            // The slot could be swept and reused between flag check and inc_ref,
            // causing inc_ref to modify the wrong object's ref count.
            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                assert!(
                    (*header.as_ptr()).is_allocated(idx),
                    "GcHandle::resolve: object slot was swept before inc_ref"
                );
            }

            // Get generation BEFORE inc_ref to detect slot reuse (bug347).
            // If the slot is swept and reused between this check and inc_ref,
            // the generation will be different after inc_ref.
            let pre_generation = gc_box.generation();

            gc_box.inc_ref();

            // Verify generation hasn't changed - if slot was reused, this will panic.
            // This prevents inc_ref from operating on the wrong object's ref count.
            assert_eq!(
                pre_generation,
                gc_box.generation(),
                "GcHandle::resolve: slot was reused between pre-check and inc_ref (generation mismatch)"
            );

            // Post-increment safety check (TOCTOU: object may have been dropped between
            // pre-check and inc_ref). Same pattern as Weak::upgrade.
            if gc_box.dropping_state() != 0
                || gc_box.has_dead_flag()
                || gc_box.is_under_construction()
            {
                GcBox::dec_ref(self.ptr.as_ptr());
                panic!("GcHandle::resolve: object was dropped after inc_ref (TOCTOU race)");
            }

            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                // Don't call dec_ref when slot swept - it may be reused (bug133)
                assert!(
                    (*header.as_ptr()).is_allocated(idx),
                    "GcHandle::resolve: object slot was swept after inc_ref"
                );
            }

            Gc::from_raw(self.ptr.as_ptr() as *const u8)
        }
    }

    /// Tries to resolve, returning `None` if called from the wrong thread.
    ///
    /// Returns `None` when:
    /// - Called from a thread other than the origin thread (different `ThreadId`), or
    /// - The handle has been unregistered.
    ///
    /// When the origin thread has terminated, resolution can still succeed from a
    /// thread with the same `ThreadId` (reuse) via the orphan root path.
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
    #[allow(clippy::significant_drop_tightening)] // Lock held through inc_ref
    pub fn try_resolve(&self) -> Option<Gc<T>> {
        if self.handle_id == HandleId::INVALID {
            return None;
        }
        if std::thread::current().id() != self.origin_thread {
            return None;
        }
        // TCB alive: use TCB roots. TCB dead: use orphan roots (same as resolve).
        // Hold lock during check+use to prevent TOCTOU with unregister.
        self.origin_tcb.upgrade().map_or_else(
            || {
                let orphan = heap::lock_orphan_roots();
                if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
                    return None;
                }
                self.try_resolve_impl()
            },
            |tcb| {
                let roots = tcb.cross_thread_roots.lock().unwrap();
                if roots.strong.contains_key(&self.handle_id) {
                    return self.try_resolve_impl();
                }
                drop(roots);
                let orphan = heap::lock_orphan_roots();
                if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
                    return self.try_resolve_impl();
                }
                drop(orphan);
                let roots = tcb.cross_thread_roots.lock().unwrap();
                if roots.strong.contains_key(&self.handle_id) {
                    return self.try_resolve_impl();
                }
                drop(roots);
                if self.origin_tcb.upgrade().is_some() {
                    let orphan = heap::lock_orphan_roots();
                    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
                        return self.try_resolve_impl();
                    }
                }
                None
            },
        )
    }

    /// Shared `try_resolve` logic. Caller must hold TCB roots lock or orphan roots lock.
    #[inline]
    #[allow(clippy::significant_drop_tightening)]
    fn try_resolve_impl(&self) -> Option<Gc<T>> {
        unsafe {
            // FIX bug388: Check is_allocated BEFORE dereferencing to avoid type confusion.
            // If slot is swept and reused, we'd read flags from the wrong object.
            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    return None;
                }
            }

            let gc_box = &*self.ptr.as_ptr();
            if gc_box.is_under_construction()
                || gc_box.has_dead_flag()
                || gc_box.dropping_state() != 0
            {
                return None;
            }

            // Check is_allocated BEFORE inc_ref to avoid TOCTOU (bug345).
            // The slot could be swept and reused between flag check and inc_ref,
            // causing inc_ref to modify the wrong object's ref count.
            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    return None;
                }
            }

            // Get generation BEFORE inc_ref to detect slot reuse (bug347).
            // If the slot is swept and reused between this check and inc_ref,
            // the generation will be different after inc_ref.
            let pre_generation = gc_box.generation();

            gc_box.inc_ref();

            // Verify generation hasn't changed - if slot was reused, return None.
            // This prevents inc_ref from operating on the wrong object's ref count.
            if pre_generation != gc_box.generation() {
                GcBox::dec_ref(self.ptr.as_ptr());
                return None;
            }

            // Post-increment safety check (TOCTOU). Same pattern as Weak::try_upgrade.
            if gc_box.dropping_state() != 0
                || gc_box.has_dead_flag()
                || gc_box.is_under_construction()
            {
                GcBox::dec_ref(self.ptr.as_ptr());
                return None;
            }

            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    GcBox::dec_ref(self.ptr.as_ptr());
                    return None;
                }
            }

            Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
        }
    }

    /// Downgrades to a weak cross-thread handle.
    ///
    /// # Panics
    ///
    /// Panics if the object is dead or in dropping state.
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
        assert!(
            self.handle_id != HandleId::INVALID,
            "GcHandle::downgrade: cannot downgrade an unregistered GcHandle"
        );
        // Hold lock during check-and-inc_weak to prevent TOCTOU with unregister/drop.
        // Same pattern as GcHandle::clone() and GcHandle::resolve().
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let roots = tcb.cross_thread_roots.lock().unwrap();
            if !roots.strong.contains_key(&self.handle_id) {
                panic!("GcHandle::downgrade: handle has been unregistered");
            }
            unsafe {
                // Get generation BEFORE inc_weak to detect slot reuse (bug351).
                let pre_generation = (*self.ptr.as_ptr()).generation();

                (*self.ptr.as_ptr()).inc_weak();

                // Verify generation hasn't changed - if slot was reused, undo inc_weak.
                if pre_generation != (*self.ptr.as_ptr()).generation() {
                    (*self.ptr.as_ptr()).dec_weak();
                    drop(roots);
                    return WeakCrossThreadHandle {
                        weak: GcBoxWeakRef::null(),
                        origin_tcb: Weak::clone(&self.origin_tcb),
                        origin_thread: self.origin_thread,
                    };
                }

                if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8)
                {
                    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                    if !(*header.as_ptr()).is_allocated(idx) {
                        (*self.ptr.as_ptr()).dec_weak();
                        drop(roots);
                        return WeakCrossThreadHandle {
                            weak: GcBoxWeakRef::null(),
                            origin_tcb: Weak::clone(&self.origin_tcb),
                            origin_thread: self.origin_thread,
                        };
                    }
                }
                let gc_box = &*self.ptr.as_ptr();
                if gc_box.has_dead_flag()
                    || gc_box.dropping_state() != 0
                    || gc_box.is_under_construction()
                {
                    (*self.ptr.as_ptr()).dec_weak();
                    panic!(
                        "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
                    );
                }
            }
            drop(roots);
        } else {
            let orphan = heap::lock_orphan_roots();
            if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
                panic!("GcHandle::downgrade: handle has been unregistered");
            }
            unsafe {
                // Get generation BEFORE inc_weak to detect slot reuse (bug351).
                let pre_generation = (*self.ptr.as_ptr()).generation();

                (*self.ptr.as_ptr()).inc_weak();

                // Verify generation hasn't changed - if slot was reused, undo inc_weak.
                if pre_generation != (*self.ptr.as_ptr()).generation() {
                    (*self.ptr.as_ptr()).dec_weak();
                    drop(orphan);
                    return WeakCrossThreadHandle {
                        weak: GcBoxWeakRef::null(),
                        origin_tcb: Weak::clone(&self.origin_tcb),
                        origin_thread: self.origin_thread,
                    };
                }

                if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8)
                {
                    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                    if !(*header.as_ptr()).is_allocated(idx) {
                        (*self.ptr.as_ptr()).dec_weak();
                        drop(orphan);
                        return WeakCrossThreadHandle {
                            weak: GcBoxWeakRef::null(),
                            origin_tcb: Weak::clone(&self.origin_tcb),
                            origin_thread: self.origin_thread,
                        };
                    }
                }
                let gc_box = &*self.ptr.as_ptr();
                if gc_box.has_dead_flag()
                    || gc_box.dropping_state() != 0
                    || gc_box.is_under_construction()
                {
                    (*self.ptr.as_ptr()).dec_weak();
                    panic!(
                        "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle (orphan)"
                    );
                }
            }
            drop(orphan);
        }
        WeakCrossThreadHandle {
            weak: GcBoxWeakRef::new(self.ptr),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        }
    }
}

/// Removes orphan root entry and `dec_ref`s on drop (panic path).
/// Call `std::mem::forget` to prevent cleanup when returning successfully.
/// Prevents dangling root entry when panic occurs after `clone_orphan_root_with_inc_ref`
/// returns but before `GcHandle` is constructed.
struct OrphanRootRemoveGuard {
    thread_id: ThreadId,
    handle_id: HandleId,
    ptr: NonNull<GcBox<()>>,
}

impl Drop for OrphanRootRemoveGuard {
    fn drop(&mut self) {
        if heap::remove_orphan_root(self.thread_id, self.handle_id).is_some() {
            GcBox::dec_ref(self.ptr.as_ptr());
        }
    }
}

/// Removes TCB root entry and `dec_ref`s on drop (panic path).
/// Call `std::mem::forget` to prevent cleanup when returning successfully.
/// Prevents dangling root entry when panic occurs after insert but before `GcHandle` is constructed.
pub struct TcbRootRemoveGuard {
    pub(crate) tcb: Arc<ThreadControlBlock>,
    pub(crate) handle_id: HandleId,
    pub(crate) ptr: NonNull<GcBox<()>>,
}

impl Drop for TcbRootRemoveGuard {
    fn drop(&mut self) {
        let mut roots = self.tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
        GcBox::dec_ref(self.ptr.as_ptr());
    }
}

#[allow(clippy::significant_drop_tightening)] // Lock must be held through inc_ref
impl<T: Trace + 'static> Clone for GcHandle<T> {
    #[track_caller]
    fn clone(&self) -> Self {
        if self.handle_id == HandleId::INVALID {
            panic!("cannot clone an unregistered GcHandle");
        }
        // Try orphan path first. When the origin thread exits, roots are migrated to the orphan
        // table before the TCB is dropped. There is a window where upgrade() still returns Some
        // (TCB not yet dropped) but roots.strong is empty (already migrated). Trying orphan
        // first handles this race and allows clone from any thread after origin has terminated.
        let (new_id, origin_tcb) = if let (new_id, true) = heap::clone_orphan_root_with_inc_ref(
            self.origin_thread,
            self.handle_id,
            self.ptr.cast::<GcBox<()>>(),
        ) {
            let guard = OrphanRootRemoveGuard {
                thread_id: self.origin_thread,
                handle_id: new_id,
                ptr: self.ptr.cast::<GcBox<()>>(),
            };
            let result = (new_id, Weak::clone(&self.origin_tcb));
            std::mem::forget(guard);
            result
        } else if let Some(tcb) = self.origin_tcb.upgrade() {
            assert_eq!(
                std::thread::current().id(),
                self.origin_thread,
                "GcHandle::clone() must be called on the origin thread. \
                     Clone from a different thread is not allowed."
            );
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            if !roots.strong.contains_key(&self.handle_id) {
                panic!("cannot clone an unregistered GcHandle");
            }
            // All checks before inc_ref/insert: avoid orphaned root entry if any assert panics.
            // Order: is_allocated check -> inc_ref -> insert. If inc_ref or insert panics,
            // no root entry exists; the extra ref would leak but no UAF from orphaned root.
            // (matches Gc::cross_thread_handle: inc_ref before insert)
            unsafe {
                // FIX bug383: Check is_allocated BEFORE dereference to avoid TOCTOU.
                // If slot is swept and reused between dereference and check, we'd read
                // fields from the wrong object (type confusion).
                if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8)
                {
                    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                    assert!(
                        (*header.as_ptr()).is_allocated(idx),
                        "GcHandle::clone: object slot was swept before dereference"
                    );
                }

                let gc_box = &*self.ptr.as_ptr();
                assert!(
                        !gc_box.has_dead_flag()
                            && gc_box.dropping_state() == 0
                            && !gc_box.is_under_construction(),
                        "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle"
                    );
                // Get generation BEFORE inc_ref to detect slot reuse.
                // If slot is swept and reused between check and inc_ref,
                // the generation will be different after inc_ref.
                let pre_generation = (*self.ptr.as_ptr()).generation();

                (*self.ptr.as_ptr()).inc_ref();

                // Verify generation hasn't changed - if slot was reused, undo inc_ref.
                if pre_generation != (*self.ptr.as_ptr()).generation() {
                    crate::ptr::GcBox::undo_inc_ref(self.ptr.as_ptr());
                    panic!("GcHandle::clone: slot was reused during clone (generation mismatch)");
                }
            }
            let new_id = roots.allocate_id();
            roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
            let guard = TcbRootRemoveGuard {
                tcb: Arc::clone(&tcb),
                handle_id: new_id,
                ptr: self.ptr.cast::<GcBox<()>>(),
            };
            drop(roots);
            let result = (new_id, Arc::downgrade(&tcb));
            std::mem::forget(guard);
            result
        } else {
            panic!("cannot clone an unregistered GcHandle");
        };

        Self {
            ptr: self.ptr,
            origin_tcb,
            origin_thread: self.origin_thread,
            handle_id: new_id,
        }
    }
}

impl<T: Trace + 'static> Drop for GcHandle<T> {
    /// Unregisters the root entry and releases the reference count held by this handle.
    ///
    /// Safe to call from any thread: the TCB is held via `Weak`, and the root
    /// list (or orphan table) is `Mutex`-protected. No thread-local storage is accessed.
    fn drop(&mut self) {
        if self.handle_id == HandleId::INVALID {
            return;
        }
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.remove(&self.handle_id);
            drop(roots);
        } else {
            let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
        }
        self.handle_id = HandleId::INVALID;

        unsafe {
            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    return;
                }
            }
        }
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
    pub(crate) origin_tcb: Weak<ThreadControlBlock>,
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

    /// Returns `true` if `resolve()` / `try_resolve()` would succeed.
    ///
    /// This checks whether the origin thread is still alive and the underlying
    /// object is still alive and not being dropped. Consistent with
    /// `resolve()` and `try_resolve()`: returns `false` if the origin thread
    /// has terminated.
    ///
    /// Note that even if `is_valid()` returns `true`, another thread may
    /// collect the object immediately after this call returns. Use
    /// `resolve()` or `try_resolve()` to atomically obtain a strong reference.
    ///
    /// Can be called from any thread (doesn't access `T`).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.origin_tcb.upgrade().is_none() {
            return false;
        }
        self.weak.is_live()
    }

    /// Resolves to a `Gc<T>` if the object is still alive.
    ///
    /// # Safety
    ///
    /// Must be called from the origin thread. `T` may be `!Send`.
    ///
    /// # Panics
    ///
    /// Panics if called from a thread other than the origin thread (including
    /// when the origin thread has terminated). Use [`try_resolve()`] for
    /// fallible resolution.
    #[track_caller]
    pub fn resolve(&self) -> Option<Gc<T>> {
        // Check TCB liveness BEFORE the ThreadId comparison to prevent ThreadId
        // reuse from bypassing origin-thread affinity after the thread terminates.
        if self.origin_tcb.upgrade().is_none() {
            panic!(
                "WeakCrossThreadHandle::resolve: origin thread has terminated (origin={:?}). \
                 Use try_resolve() instead.",
                self.origin_thread
            );
        }
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "WeakCrossThreadHandle::resolve() must be called on the origin thread. \
             If the origin thread has terminated, use try_resolve() instead."
        );
        // Weak handle does not prevent collection. Check liveness first.
        self.weak.upgrade()
    }

    /// Tries to resolve, returning `None` if called from wrong thread
    /// or if the object has been collected.
    ///
    /// Use this when the origin thread may have terminated.
    #[must_use]
    pub fn try_resolve(&self) -> Option<Gc<T>> {
        // Check TCB liveness BEFORE the ThreadId comparison. `ThreadId`s can be reused
        // after thread termination, so a new thread with the same `ThreadId` would
        // otherwise bypass the origin-thread check.
        self.origin_tcb.upgrade()?;
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
    ///
    /// Note: Returns `false` if the origin thread has terminated.
    #[inline]
    #[must_use]
    pub fn may_be_valid(&self) -> bool {
        if self.origin_tcb.upgrade().is_none() {
            return false;
        }
        self.weak.may_be_valid()
    }

    /// Attempt to upgrade with additional safety checks.
    ///
    /// Returns `None` if:
    /// - The weak ref is null
    /// - The object has been collected
    /// - The memory location is obviously invalid (misaligned or too low address)
    /// - The origin thread has terminated
    ///
    /// # Safety
    ///
    /// Must be called from the origin thread. `T` may be `!Send`.
    ///
    /// # Panics
    ///
    /// Panics if called from a live thread other than the origin thread.
    #[track_caller]
    pub fn try_upgrade(&self) -> Option<Gc<T>> {
        // Check TCB liveness BEFORE the ThreadId comparison to prevent ThreadId
        // reuse from bypassing origin-thread affinity after the thread terminates.
        // Returns None (rather than panicking) if origin thread has terminated,
        // consistent with try_resolve().
        self.origin_tcb.upgrade()?;
        assert_eq!(
            std::thread::current().id(),
            self.origin_thread,
            "WeakCrossThreadHandle::try_upgrade() must be called on the origin thread. \
             If the origin thread has terminated, use try_resolve() instead."
        );
        self.weak.try_upgrade()
    }
}

impl<T: Trace + 'static> Clone for WeakCrossThreadHandle<T> {
    #[track_caller]
    fn clone(&self) -> Self {
        // Clone is allowed from any thread. The weak ref does not register roots or expose T;
        // try_resolve/resolve enforce origin-thread affinity when actually accessing the value.
        // This matches GcHandle::clone behavior when origin has terminated (bug156), and avoids
        // a race where join() returns before TCB is dropped (upgrade still Some).
        Self {
            weak: self.weak.clone(),
            origin_tcb: Weak::clone(&self.origin_tcb),
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
        let ptr_addr = ptr.as_ptr() as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {
            return;
        }
        unsafe {
            // Check generation to detect slot reuse (bug231 fix: avoid UAF/corruption).
            // bug402 fix: use generation check instead of is_allocated to avoid weak ref leak.
            let current_generation = (*ptr.as_ptr()).generation();
            if current_generation != self.weak.generation() {
                // Slot was reused - skip dec_weak_raw to avoid corrupting new GcBox's weak count
                return;
            }
            // SAFETY: Use dec_weak_raw to avoid creating a reference to the GcBox.
            // When WeakCrossThreadHandle::drop runs during drop of a value inside a GcBox,
            // the value field is under drop_in_place. (*ptr.as_ptr()).dec_weak() would
            // violate Stacked Borrows. dec_weak_raw uses addr_of! internally (bug265).
            let _ = GcBox::dec_weak_raw(ptr.as_ptr().cast::<GcBox<()>>());
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
