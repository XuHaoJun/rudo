//! The `Gc<T>` smart pointer implementation.
//!
//! This module provides the primary user-facing type for garbage-collected
//! memory management.

#![allow(clippy::ptr_as_ptr, clippy::ptr_cast_constness)]

use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::gc::incremental::mark_new_object_black;
use crate::gc::notify_dropped_gc;
use crate::heap::{with_heap, LocalHeap};
use crate::trace::{GcVisitor, Trace, Visitor};

// ============================================================================
// `GcBox` - The heap allocation container
// ============================================================================

/// The actual heap allocation wrapping the user's value.
#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    /// Current reference count (for amortized collection triggering).
    /// Uses `AtomicUsize` for thread-safe reference counting.
    ref_count: AtomicUsize,
    /// Number of weak references to this allocation.
    /// Uses `AtomicUsize` for thread-safe weak reference counting.
    weak_count: AtomicUsize,
    /// Type-erased destructor for the value.
    pub(crate) drop_fn: unsafe fn(*mut u8),
    /// Type-erased trace function for the value.
    pub(crate) trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    /// Flag indicating the object is being dropped (prevents `weak::upgrade` race).
    is_dropping: AtomicUsize,
    /// The user's data.
    value: T,
}

impl<T: Trace + ?Sized> GcBox<T> {
    /// Bit mask for the "value dead" flag (highest bit).
    const DEAD_FLAG: usize = 1 << (usize::BITS - 1);
    /// Bit mask for the "under construction" flag (second highest bit).
    const UNDER_CONSTRUCTION_FLAG: usize = 1 << (usize::BITS - 2);
    /// Combined mask for all flags stored in `weak_count`.
    const FLAGS_MASK: usize = Self::DEAD_FLAG | Self::UNDER_CONSTRUCTION_FLAG;

    /// Get the reference count.
    /// Uses Acquire ordering to ensure we see the complete effect of any
    /// prior decrements, preventing use-after-free.
    pub fn ref_count(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.ref_count.load(Ordering::Acquire))
            .expect("ref_count should never be zero for live GcBox")
    }

    /// Check if the value is currently under construction.
    #[inline]
    fn is_under_construction(&self) -> bool {
        (self.weak_count.load(Ordering::Relaxed) & Self::UNDER_CONSTRUCTION_FLAG) != 0
    }

    /// Set the under-construction flag.
    /// Uses `Release` ordering to synchronize the flag with value construction completion.
    /// Uses `AcqRel` when clearing to synchronize with concurrent readers.
    #[inline]
    fn set_under_construction(&self, flag: bool) {
        let mask = Self::UNDER_CONSTRUCTION_FLAG;
        if flag {
            self.weak_count.fetch_or(mask, Ordering::Release);
        } else {
            self.weak_count.fetch_and(!mask, Ordering::AcqRel);
        }
    }

    /// Check if the value is currently being dropped (prevents `weak::upgrade` race).
    /// Returns the dropping state: 0 = not dropping, 1 = dropping phase 1, 2 = final dropping.
    /// Uses `Acquire` ordering to synchronize with `try_mark_dropping()`.
    #[inline]
    fn dropping_state(&self) -> usize {
        self.is_dropping.load(Ordering::Acquire)
    }

    /// Try to mark the value as dropping. Returns true if successful.
    /// Uses CAS to prevent races with concurrent upgrade.
    /// The failure ordering is Acquire to ensure we see other threads' state changes.
    #[inline]
    fn try_mark_dropping(&self) -> bool {
        self.is_dropping
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Mark the value as in final dropping phase (after value drop started).
    /// This is used to distinguish between:
    /// - Phase 1: value drop in progress (can still safely access nested Gc)
    /// - Phase 2: after value drop completed (prevent reentrancy)
    #[inline]
    unsafe fn set_final_dropping(&self) {
        self.is_dropping.store(2, Ordering::Release);
    }

    /// Increment the reference count.
    /// Uses Relaxed ordering since this is just a counter increment.
    pub fn inc_ref(&self) {
        // Saturating add to prevent overflow - saturates at isize::MAX
        self.ref_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                if count == usize::MAX {
                    None // Stay at MAX
                } else {
                    Some(count.saturating_add(1))
                }
            })
            .ok();
    }

    /// Decrement the reference count. Returns true if count reached zero.
    /// Uses `AcqRel` ordering to synchronize with other threads.
    /// Takes a raw pointer to avoid Miri's stacked borrows issues with `&self` casting.
    pub fn dec_ref(self_ptr: *mut Self) -> bool {
        // SAFETY: self_ptr is valid because it's obtained from the atomic pointer in Gc::drop
        let this = unsafe { &*self_ptr };
        loop {
            let dead_flag = this.weak_count_raw() & GcBox::<()>::DEAD_FLAG;
            if dead_flag != 0 {
                // Already marked as dead (e.g., during sweep phase drop of cyclic refs).
                // Return false to prevent double-drop: the drop_fn was/will be called
                // by the sweep phase, not by this dec_ref.
                return false;
            }

            let count = this.ref_count.load(Ordering::Acquire);
            if count == 0 {
                // Already at zero - this is a bug (double-free or use-after-free)
                // Return true to prevent further issues
                return true;
            }
            if count == 1 && this.dropping_state() == 0 {
                // Last reference and not already marked as dropping
                // Must mark as dropping BEFORE dropping to prevent
                // race with concurrent Weak::upgrade
                if this.try_mark_dropping() {
                    // SAFETY: We're the last reference and marked as dropping,
                    // safe to drop. The drop function handles value dropping.
                    unsafe {
                        (this.drop_fn)(self_ptr.cast::<u8>());
                    }
                    return true;
                }
                // CAS failed - another thread beat us to marking
                // Fall through to retry loop
            }
            // Attempt to decrement
            if this
                .ref_count
                .compare_exchange_weak(count, count - 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return false;
            }
            // Contention - retry
        }
    }

    /// Get a reference to the value.
    #[allow(dead_code)]
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Get the weak reference count.
    pub fn weak_count(&self) -> usize {
        self.weak_count.load(Ordering::Relaxed) & !Self::FLAGS_MASK
    }

    /// Get the raw weak count value (including flags).
    pub fn weak_count_raw(&self) -> usize {
        self.weak_count.load(Ordering::Relaxed)
    }

    /// Increment the weak reference count.
    /// Uses Relaxed ordering since weak count is advisory only.
    pub fn inc_weak(&self) {
        let current = self.weak_count.load(Ordering::Relaxed);
        let flags = current & Self::FLAGS_MASK;
        let count = current & !Self::FLAGS_MASK;
        let new_count = count.saturating_add(1);
        self.weak_count.store(flags | new_count, Ordering::Relaxed);
    }

    /// Decrement the weak reference count. Returns true if count reached zero.
    /// Uses `AcqRel` ordering to synchronize weak count changes.
    pub fn dec_weak(&self) -> bool {
        loop {
            let current = self.weak_count.load(Ordering::Relaxed);
            let flags = current & Self::FLAGS_MASK;
            let count = current & !Self::FLAGS_MASK;

            if count == 0 {
                return true;
            } else if count == 1 {
                // Attempt to set to just flags
                match self.weak_count.compare_exchange_weak(
                    current,
                    flags,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(_) => continue,
                }
            }
            // Attempt to decrement
            if self
                .weak_count
                .compare_exchange_weak(
                    current,
                    flags | (count - 1),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return false;
            }
        }
    }

    /// Check if the value has been dropped (only weak refs remain).
    pub fn is_value_dead(&self) -> bool {
        (self.weak_count.load(Ordering::Relaxed) & Self::DEAD_FLAG) != 0
    }

    /// Mark the value as dropped.
    pub(crate) fn set_dead(&self) {
        self.weak_count.fetch_or(Self::DEAD_FLAG, Ordering::Relaxed);
    }

    /// Check if the value has been collected (dead).
    pub(crate) fn is_dead(&self) -> bool {
        // Value is dead if dead flag is set OR ref count is zero
        (self.weak_count.load(Ordering::Acquire) & Self::DEAD_FLAG) != 0
            || self.ref_count.load(Ordering::Acquire) == 0
    }

    /// Mark the value as dead during panic cleanup.
    /// Clears `UNDER_CONSTRUCTION_FLAG` and sets `DEAD_FLAG`.
    /// Used when weak references exist and we can't deallocate.
    pub(crate) fn mark_dead(&self) {
        self.weak_count.fetch_or(Self::DEAD_FLAG, Ordering::Relaxed);
        self.weak_count
            .fetch_and(!Self::UNDER_CONSTRUCTION_FLAG, Ordering::Relaxed);
    }
}

impl<T: Trace> GcBox<T> {
    /// Type-erased drop function for any Sized T.
    pub(crate) unsafe fn drop_fn_for(ptr: *mut u8) {
        // SAFETY: The caller must ensure ptr points to a GcBox<T> where T: Sized.
        // This is true for all objects allocated via Gc::new.
        let gc_box = ptr.cast::<Self>();

        unsafe {
            // Check if already in final dropping phase (prevents reentrancy during
            // cyclic reference drops in sweep phase). If dropping_state >= 2,
            // we've already dropped the value and are being called again from
            // nested Gc::drop during the drop of another object in the cycle.
            if (*gc_box).dropping_state() >= 2 {
                return;
            }

            // Set dead flag BEFORE dropping the value to prevent reentrancy.
            // During cyclic reference drops, nested Gc::drop calls dec_ref,
            // which checks the dead flag and returns early if set.
            (*gc_box).set_dead();

            std::ptr::drop_in_place(std::ptr::addr_of_mut!((*gc_box).value));

            // Mark as in final dropping phase AFTER value is dropped.
            // This prevents any further reentrancy attempts.
            (*gc_box).set_final_dropping();
            (*gc_box).drop_fn = GcBox::<()>::no_op_drop;
            (*gc_box).trace_fn = GcBox::<()>::no_op_trace;
        }
    }

    /// Type-erased trace function for any Sized T.
    pub(crate) unsafe fn trace_fn_for(ptr: *const u8, visitor: &mut GcVisitor) {
        let gc_box = ptr.cast::<Self>();
        // SAFETY: The caller ensures ptr points to a valid GcBox<T>
        unsafe {
            (*gc_box).value.trace(visitor);
        }
    }

    /// Create a weak reference to this `GcBox`.
    #[allow(dead_code)]
    pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
        GcBoxWeakRef::new(NonNull::from(self))
    }
}

impl GcBox<()> {
    /// A no-op drop function for already-dropped objects.
    pub(crate) const unsafe fn no_op_drop(_ptr: *mut u8) {}

    /// A no-op trace function for already-dropped objects.
    pub(crate) const unsafe fn no_op_trace(_ptr: *const u8, _visitor: &mut GcVisitor) {}
}

/// Internal weak reference type for cross-thread handles.
///
/// This is similar to `Weak<T>` but without the `Trace` bound since it's
/// only used internally by the GC system.
#[allow(clippy::redundant_pub_crate)]
pub(crate) struct GcBoxWeakRef<T: Trace + 'static> {
    ptr: AtomicNullable<GcBox<T>>,
}

impl<T: Trace + 'static> GcBoxWeakRef<T> {
    /// Create a new weak reference.
    pub(crate) fn new(ptr: NonNull<GcBox<T>>) -> Self {
        Self {
            ptr: AtomicNullable::new(ptr),
        }
    }

    /// Upgrade to a strong reference.
    pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
        let ptr = self.ptr.load(Ordering::Acquire).as_option()?;

        unsafe {
            let gc_box = &*ptr.as_ptr();
            // Check if value is dead (collected)
            if gc_box.is_dead() {
                return None;
            }
            // Increment ref count
            gc_box.inc_ref();
            Some(Gc {
                ptr: AtomicNullable::new(ptr),
                _marker: PhantomData,
            })
        }
    }

    /// Clone the weak reference.
    pub(crate) fn clone(&self) -> Self {
        Self {
            ptr: AtomicNullable::new(self.ptr.load(Ordering::Acquire).as_option().unwrap()),
        }
    }
}

#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + 'static> Send for GcBoxWeakRef<T> {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + 'static> Sync for GcBoxWeakRef<T> {}

// ============================================================================
// Nullable - A nullable pointer to unsized types
// ============================================================================

/// A nullable pointer for `?Sized` types.
#[derive(Debug)]
pub struct Nullable<T: ?Sized>(*mut T);

#[allow(dead_code)]
impl<T: ?Sized> Nullable<T> {
    /// Create a new nullable pointer from a non-null pointer.
    #[must_use]
    pub const fn new(ptr: NonNull<T>) -> Self {
        Self(ptr.as_ptr())
    }

    /// Create a null pointer.
    pub const fn null() -> Self
    where
        T: Sized,
    {
        Self(std::ptr::null_mut())
    }

    /// Convert this to a null pointer (preserving metadata for unsized types).
    #[must_use]
    pub fn as_null(self) -> Self {
        Self(self.0.with_addr(0))
    }

    /// Check if this pointer is null.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.0.is_null() || (self.0 as *const () as usize) == 0
    }

    /// Convert to Option<`NonNull`<T>>.
    #[must_use]
    pub fn as_option(self) -> Option<NonNull<T>> {
        NonNull::new(self.0)
    }

    /// Get the raw pointer.
    #[must_use]
    pub const fn as_ptr(self) -> *mut T {
        self.0
    }

    /// Unwrap the pointer, panicking if null.
    #[must_use]
    pub fn unwrap(self) -> NonNull<T> {
        self.as_option()
            .expect("attempted to unwrap null Gc pointer")
    }

    /// Create from a raw pointer.
    #[allow(dead_code)]
    #[must_use]
    pub const fn from_ptr(ptr: *mut T) -> Self {
        Self(ptr)
    }
}

impl<T: ?Sized> Clone for Nullable<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for Nullable<T> {}

impl<T: ?Sized> PartialEq for Nullable<T> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

/// An atomic nullable pointer.
/// Uses `AtomicUsize` to store the pointer as a raw usize.
#[derive(Debug)]
pub struct AtomicNullable<T: Sized> {
    ptr: AtomicUsize,
    _marker: PhantomData<*mut T>,
}

#[allow(dead_code)]
impl<T: Sized> AtomicNullable<T> {
    /// Create a new atomic nullable pointer from a non-null pointer.
    #[must_use]
    pub fn new(ptr: NonNull<T>) -> Self {
        Self {
            ptr: AtomicUsize::new(ptr.as_ptr().cast::<()>() as usize),
            _marker: PhantomData,
        }
    }

    /// Create a null pointer.
    pub const fn null() -> Self {
        Self {
            ptr: AtomicUsize::new(0),
            _marker: PhantomData,
        }
    }

    /// Set this pointer to null.
    pub fn set_null(&self) {
        self.ptr.store(0, Ordering::Relaxed);
    }

    /// Load the value with the given ordering.
    #[must_use]
    #[allow(clippy::ptr_as_ptr)]
    pub fn load(&self, ordering: Ordering) -> Nullable<T> {
        let addr = self.ptr.load(ordering);
        Nullable::from_ptr(addr as *mut T)
    }

    /// Store a value with the given ordering.
    #[allow(clippy::ptr_as_ptr)]
    pub fn store(&self, ptr: Nullable<T>, ordering: Ordering) {
        self.ptr.store(ptr.as_ptr().cast::<()>() as usize, ordering);
    }
}

impl<T: Sized> Clone for AtomicNullable<T> {
    fn clone(&self) -> Self {
        Self {
            ptr: AtomicUsize::new(self.ptr.load(Ordering::Relaxed)),
            _marker: PhantomData,
        }
    }
}

// ============================================================================
// Gc<T> - The garbage-collected smart pointer
// ============================================================================
// Gc<T> - The garbage-collected smart pointer
// ============================================================================

/// A garbage-collected pointer to a value of type `T`.
///
/// `Gc<T>` provides shared ownership of a value, similar to `Rc<T>`, but with
/// automatic cycle detection and collection.
///
/// # Thread Safety
///
/// `Gc<T>` implements `Send` and `Sync` when `T: Send + Sync`. The reference
/// counting operations use atomic operations for thread safety.
///
/// # Panics
///
/// Dereferencing a "dead" `Gc` (one whose value has been collected during
/// a Drop implementation) will panic. Use `Gc::try_deref()` for fallible access.
///
/// # Examples
///
/// ```ignore
/// use rudo_gc::Gc;
///
/// let x = Gc::new(42);
/// assert_eq!(*x, 42);
///
/// let y = Gc::clone(&x);
/// assert!(Gc::ptr_eq(&x, &y));
/// ```
pub struct Gc<T: Trace + 'static> {
    /// Pointer to the heap-allocated box.
    /// Uses `AtomicNullable` for thread-safe access.
    ptr: AtomicNullable<GcBox<T>>,
    /// Marker to properly convey ownership semantics.
    _marker: PhantomData<*const ()>,
}

impl<T: Trace> Gc<T> {
    /// Create a new garbage-collected value.
    ///
    /// # Zero-Sized Types
    ///
    /// For zero-sized types (ZSTs) like `()`, this creates a singleton
    /// allocation that is shared across all instances.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::Gc;
    ///
    /// let x = Gc::new(42);
    /// assert_eq!(*x, 42);
    ///
    /// // ZSTs are handled efficiently
    /// let unit = Gc::new(());
    /// ```
    pub fn new(value: T) -> Self {
        // Handle Zero-Sized Types specially
        if std::mem::size_of::<T>() == 0 {
            return Self::new_zst(value);
        }

        // Allocate space in the heap
        let ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);

        // Initialize the GcBox
        let gc_box = ptr.as_ptr().cast::<GcBox<T>>();
        // SAFETY: We just allocated this memory
        unsafe {
            gc_box.write(GcBox {
                ref_count: AtomicUsize::new(1),
                weak_count: AtomicUsize::new(0),
                drop_fn: GcBox::<T>::drop_fn_for,
                trace_fn: GcBox::<T>::trace_fn_for,
                is_dropping: AtomicUsize::new(0),
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        // Mark as black (live) during incremental marking
        // This is the SATB "black allocation" optimization
        #[allow(clippy::ptr_as_ptr)]
        let _ = mark_new_object_black(ptr.as_ptr() as *const u8);

        // Notify that we created a Gc
        crate::gc::notify_created_gc();

        Self {
            ptr: AtomicNullable::new(gc_box_ptr),
            _marker: PhantomData,
        }
    }

    /// Create a Gc for a zero-sized type.
    ///
    /// ZSTs don't need heap allocation - we use a sentinel address.
    #[allow(clippy::items_after_statements, clippy::cast_ptr_alignment)]
    fn new_zst(_value: T) -> Self {
        debug_assert!(std::mem::size_of::<T>() == 0);

        // For ZSTs, we use a special sentinel address that's:
        // 1. Non-null (so we can distinguish from dead Gc)
        // 2. Aligned for GcBox<T>
        // 3. Never actually dereferenced for its value
        //
        // We allocate a minimal GcBox to hold the ZST ref count.
        // Since the value is zero-sized, this is just the ref_count field.

        // Use AtomicPtr for thread-safe lazy initialization of ZST singleton.
        // The singleton is initialized with weak_count=1, which prevents the GC
        // sweep phase from reclaiming it. This ensures the singleton address
        // remains valid for the lifetime of the program, preventing ABA issues.
        static ZST_SINGLETON: AtomicPtr<GcBox<()>> = AtomicPtr::new(std::ptr::null_mut());

        let gc_box_ptr: *mut GcBox<()> = {
            let ptr = ZST_SINGLETON.load(Ordering::Acquire);

            if ptr.is_null() {
                let alloc_ptr = with_heap(LocalHeap::alloc::<GcBox<()>>);
                let gc_box = alloc_ptr.as_ptr().cast::<GcBox<()>>();

                // SAFETY: We just allocated this memory. The value is a ZST (unit type).
                // weak_count is set to 1 to mark this as an immortal singleton that
                // should never be reclaimed by the GC sweep phase.
                unsafe {
                    gc_box.write(GcBox {
                        ref_count: AtomicUsize::new(1),
                        weak_count: AtomicUsize::new(1),
                        drop_fn: GcBox::<()>::drop_fn_for,
                        trace_fn: GcBox::<()>::trace_fn_for,
                        is_dropping: AtomicUsize::new(0),
                        value: (),
                    });
                }

                // Try to CAS our allocation into the singleton slot
                let null_ptr: *mut GcBox<()> = std::ptr::null_mut();
                if ZST_SINGLETON
                    .compare_exchange_weak(null_ptr, gc_box, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    // Another thread initialized first - use theirs and drop ours
                    // SAFETY: We're dropping the allocation we just wrote, which is safe
                    // since no one else has a reference to it yet
                    unsafe {
                        gc_box.read();
                        with_heap(|heap| heap.dealloc(alloc_ptr));
                    }
                    ZST_SINGLETON.load(Ordering::Acquire)
                } else {
                    gc_box
                }
            } else {
                ptr
            }
        };

        // SAFETY: We know this is a valid GcBox<()> for ZST
        // Increment ref count for the new Gc handle
        unsafe {
            (*gc_box_ptr).inc_ref();
        }

        let gc_box_ptr = unsafe {
            // Cast from *mut GcBox<()> to NonNull<GcBox<T>> for the return type
            NonNull::new_unchecked(gc_box_ptr.cast::<GcBox<T>>())
        };

        // Notify that we created a Gc
        crate::gc::notify_created_gc();

        Self {
            ptr: AtomicNullable::new(gc_box_ptr),
            _marker: PhantomData,
        }
    }

    #[deprecated(
        since = "0.0.1",
        note = "Self-referential cycles are not supported. Use `new_cyclic_weak` instead."
    )]
    #[allow(unused)]
    #[must_use]
    #[doc(hidden)]
    pub fn new_cyclic<F: FnOnce(Self) -> T>(data_fn: F) -> Self {
        let ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
        let gc_box = ptr.as_ptr().cast::<GcBox<T>>();

        let dead_gc = Self {
            ptr: AtomicNullable::null(),
            _marker: PhantomData,
        };

        let value = data_fn(dead_gc);

        unsafe {
            gc_box.write(GcBox {
                ref_count: AtomicUsize::new(1),
                weak_count: AtomicUsize::new(0),
                drop_fn: GcBox::<T>::drop_fn_for,
                trace_fn: GcBox::<T>::trace_fn_for,
                is_dropping: AtomicUsize::new(0),
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        let gc = Self {
            ptr: AtomicNullable::new(gc_box_ptr),
            _marker: PhantomData,
        };

        unsafe {
            rehydrate_self_refs(gc_box_ptr, &(*gc_box).value);
        }

        gc
    }

    /// Create a self-referential garbage-collected value using a Weak reference.
    ///
    /// The closure receives a `Weak<T>` that will be upgradeable after
    /// construction completes. Store this `Weak` in the constructed value
    /// and call `upgrade()` when access to the self-reference is needed.
    ///
    /// # Panics
    ///
    /// Panics if `T` is a zero-sized type (ZST).
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, Weak, Trace, GcCell};
    ///
    /// #[derive(Trace)]
    /// struct Node {
    ///     self_ref: GcCell<Option<Weak<Node>>>,
    ///     data: i32,
    /// }
    ///
    /// let node = Gc::new_cyclic_weak(|weak_self| {
    ///     Node {
    ///         self_ref: GcCell::new(Some(weak_self)),
    ///         data: 42,
    ///     }
    /// });
    ///
    /// // Access self through upgrade()
    /// let weak = node.self_ref.borrow();
    /// let self_ref = weak.as_ref().unwrap().upgrade().unwrap();
    /// assert_eq!(self_ref.data, 42);
    /// ```
    #[track_caller]
    #[allow(clippy::items_after_statements)]
    pub fn new_cyclic_weak<F>(data_fn: F) -> Self
    where
        F: FnOnce(Weak<T>) -> T,
    {
        assert!(
            std::mem::size_of::<T>() != 0,
            "Gc::new_cyclic_weak does not support zero-sized types"
        );

        struct DropGuard<T: Trace + ?Sized> {
            ptr: NonNull<u8>,
            completed: bool,
            gc_box_ptr: NonNull<GcBox<T>>,
        }

        impl<T: Trace + ?Sized> Drop for DropGuard<T> {
            fn drop(&mut self) {
                if self.completed {
                    return;
                }
                unsafe {
                    let raw_weak_count = (*self.gc_box_ptr.as_ptr()).weak_count_raw();
                    let actual_count = raw_weak_count & !GcBox::<T>::FLAGS_MASK;
                    if actual_count > 0
                        || (raw_weak_count & GcBox::<T>::UNDER_CONSTRUCTION_FLAG) != 0
                    {
                        (*self.gc_box_ptr.as_ptr()).mark_dead();
                    } else {
                        with_heap(|heap| {
                            heap.dealloc(self.ptr);
                        });
                    }
                }
            }
        }

        let raw_ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
        let gc_box = raw_ptr.as_ptr().cast::<GcBox<T>>();

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        let mut guard = DropGuard::<T> {
            ptr: raw_ptr,
            completed: false,
            gc_box_ptr,
        };

        unsafe {
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).ref_count),
                AtomicUsize::new(1),
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).weak_count),
                AtomicUsize::new(GcBox::<T>::UNDER_CONSTRUCTION_FLAG),
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).drop_fn),
                GcBox::<T>::drop_fn_for,
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).trace_fn),
                GcBox::<T>::trace_fn_for,
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).is_dropping),
                AtomicUsize::new(0),
            );
        }

        let weak_self = Weak {
            ptr: AtomicNullable::new(gc_box_ptr),
        };

        let value = data_fn(weak_self);

        unsafe {
            std::ptr::write(std::ptr::addr_of_mut!((*gc_box).value), value);
        }

        unsafe {
            (*gc_box_ptr.as_ptr()).set_under_construction(false);
        }

        guard.completed = true;
        std::mem::forget(guard);

        crate::gc::notify_created_gc();

        Self {
            ptr: AtomicNullable::new(gc_box_ptr),
            _marker: PhantomData,
        }
    }

    /// Create a `Gc<T>` from a raw pointer to its `GcBox`.
    ///
    /// # Safety
    ///
    /// The pointer must be a valid, currently allocated `GcBox<T>`.
    #[doc(hidden)]
    #[must_use]
    pub unsafe fn from_raw(ptr: *const u8) -> Self {
        Self {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(ptr as *mut GcBox<T>) }),
            _marker: PhantomData,
        }
    }
}

impl<T: Trace> Gc<T> {
    /// Attempt to dereference this `Gc`.
    ///
    /// Returns `None` if this Gc is "dead" (only possible during Drop of cycles).
    pub fn try_deref(gc: &Self) -> Option<&T> {
        let ptr = gc.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            Some(&**gc)
        }
    }

    /// Attempt to clone this `Gc`.
    ///
    /// Returns `None` if this Gc is "dead".
    pub fn try_clone(gc: &Self) -> Option<Self> {
        let ptr = gc.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            Some(gc.clone())
        }
    }

    /// Get a raw pointer to the data.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    pub fn as_ptr(gc: &Self) -> *const T {
        let ptr = gc.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        // SAFETY: ptr is not null (checked in callers), and ptr is valid
        unsafe { std::ptr::addr_of!((*gc_box_ptr).value) }
    }

    /// Get the internal `GcBox` pointer.
    pub fn internal_ptr(gc: &Self) -> *const u8 {
        gc.ptr.load(Ordering::Acquire).as_ptr() as *const u8
    }

    /// Check if two Gcs point to the same allocation.
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.ptr.load(Ordering::Acquire).as_ptr() == other.ptr.load(Ordering::Acquire).as_ptr()
    }

    /// Get the current reference count.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    pub fn ref_count(gc: &Self) -> NonZeroUsize {
        let ptr = gc.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        // SAFETY: ptr is not null (checked in callers)
        unsafe { (*gc_box_ptr).ref_count() }
    }

    /// Get the current weak reference count.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    pub fn weak_count(gc: &Self) -> usize {
        let ptr = gc.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        // SAFETY: ptr is not null (checked in callers)
        unsafe { (*gc_box_ptr).weak_count() }
    }

    /// Create a `Weak<T>` pointer to this allocation.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::{Gc, Weak};
    ///
    /// let gc = Gc::new(42);
    /// let weak = Gc::downgrade(&gc);
    ///
    /// assert!(weak.upgrade().is_some());
    ///
    /// drop(gc);
    /// // After collection, the weak reference cannot upgrade
    /// ```
    pub fn downgrade(gc: &Self) -> Weak<T> {
        let ptr = gc.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        // Increment the weak count
        // SAFETY: ptr is valid and not null
        unsafe {
            (*gc_box_ptr).inc_weak();
        }
        Weak {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
        }
    }

    /// Check if this Gc is "dead" (refers to a collected value).
    pub fn is_dead(gc: &Self) -> bool {
        gc.ptr.load(Ordering::Acquire).is_null()
    }

    /// Kill this Gc, making it dead.
    #[allow(dead_code)]
    pub(crate) fn kill(&self) {
        self.ptr.set_null();
    }

    /// Get the raw `GcBox` pointer.
    pub(crate) fn raw_ptr(&self) -> *mut GcBox<T> {
        self.ptr.load(Ordering::Acquire).as_ptr()
    }

    /// Get a `NonNull` pointer to the `GcBox`.
    pub(crate) fn as_non_null(&self) -> NonNull<GcBox<T>> {
        self.ptr.load(Ordering::Acquire).as_option().unwrap()
    }

    /// Get a weak reference to this GC allocation.
    #[allow(dead_code)]
    pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
        let ptr = self.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        // Increment the weak count
        // SAFETY: ptr is valid and not null
        unsafe {
            (*gc_box_ptr).inc_weak();
        }
        GcBoxWeakRef {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
        }
    }
}

impl<T: Trace + 'static> Gc<T> {
    /// Creates a cross-thread handle to this GC object.
    ///
    /// The handle is `Send + Sync` and can be sent to any thread.
    /// Call `resolve()` on the creating thread to obtain a local `Gc<T>`.
    ///
    /// The object will not be collected while any strong handle to it exists.
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
    /// // Handle can be sent to another thread
    /// ```
    #[must_use]
    pub fn cross_thread_handle(&self) -> crate::handles::GcHandle<T> {
        use std::sync::Arc;

        use crate::handles::GcHandle;

        let tcb = crate::heap::current_thread_control_block()
            .expect("cross_thread_handle called outside of GC context");

        // Lock the root table BEFORE reading the pointer.
        // While this lock is held, GC cannot sweep this thread's
        // cross-thread roots (GC also acquires this lock for marking).
        let mut roots = tcb.cross_thread_roots.lock().unwrap();
        let handle_id = roots.allocate_id();

        let ptr = self.as_non_null();
        roots.strong.insert(handle_id, ptr.cast::<GcBox<()>>());

        drop(roots);

        GcHandle {
            ptr,
            origin_tcb: Arc::clone(&tcb),
            origin_thread: std::thread::current().id(),
            handle_id,
        }
    }

    /// Creates a weak cross-thread handle that doesn't prevent collection.
    ///
    /// Resolve returns `None` if the object has been collected.
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
    /// // Weak doesn't keep the object alive
    /// ```
    #[must_use]
    pub fn weak_cross_thread_handle(&self) -> crate::handles::WeakCrossThreadHandle<T> {
        crate::handles::WeakCrossThreadHandle {
            weak: GcBoxWeakRef::new(self.as_non_null()),
            origin_tcb: crate::heap::current_thread_control_block()
                .expect("weak_cross_thread_handle called outside of GC context"),
            origin_thread: std::thread::current().id(),
        }
    }
}

impl<T: Trace> Deref for Gc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        let ptr = self.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        // SAFETY: ptr is not null (checked in callers), and ptr is valid
        unsafe { &(*gc_box_ptr).value }
    }
}

impl<T: Trace> Clone for Gc<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return Self {
                ptr: AtomicNullable::null(),
                _marker: PhantomData,
            };
        }

        let gc_box_ptr = ptr.as_ptr();

        // Increment reference count
        // SAFETY: Pointer is valid (not null)
        unsafe {
            (*gc_box_ptr).inc_ref();
        }

        Self {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
            _marker: PhantomData,
        }
    }
}

impl<T: Trace> Drop for Gc<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return;
        }

        let gc_box_ptr = ptr.as_ptr();

        let is_last = GcBox::<T>::dec_ref(gc_box_ptr);

        if is_last {
            unsafe {
                ((*gc_box_ptr).drop_fn)(gc_box_ptr.cast::<u8>());
            }
        } else {
            notify_dropped_gc();
        }
    }
}

impl<T: Trace + PartialEq> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Trace + Eq> Eq for Gc<T> {}

impl<T: Trace + std::fmt::Debug> std::fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ptr.load(Ordering::Acquire).is_null() {
            write!(f, "Gc(<dead>)")
        } else {
            f.debug_tuple("Gc").field(&&**self).finish()
        }
    }
}

impl<T: Trace> std::fmt::Pointer for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Pointer::fmt(&self.ptr.load(Ordering::Acquire).as_ptr(), f)
    }
}

impl<T: Trace + std::fmt::Display> std::fmt::Display for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&**self, f)
    }
}

impl<T: Trace + Default> Default for Gc<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Trace + std::hash::Hash> std::hash::Hash for Gc<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

impl<T: Trace + PartialOrd> PartialOrd for Gc<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: Trace + Ord> Ord for Gc<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: Trace> From<T> for Gc<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Trace> AsRef<T> for Gc<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: Trace> std::borrow::Borrow<T> for Gc<T> {
    fn borrow(&self) -> &T {
        self
    }
}

// Gc is NOT Send or Sync
// We use PhantomData<*const ()> to ensure this, which is !Send and !Sync.
// The marker is already in the struct, so these impls are not needed.
// Note: Negative trait impls require nightly, so we rely on the marker type instead.

// ============================================================================
// Weak<T> - Weak reference to a garbage-collected value
// ============================================================================

/// A weak reference to a garbage-collected value.
///
/// `Weak<T>` does not keep the value alive. Use `upgrade()` to get a `Gc<T>`
/// if the value still exists.
///
/// Unlike strong `Gc<T>` references, weak references do not prevent garbage
/// collection. After the value is collected, `upgrade()` will return `None`.
///
/// # Examples
///
/// ```ignore
/// use rudo_gc::{Gc, Weak};
///
/// let strong = Gc::new(42);
/// let weak = Gc::downgrade(&strong);
///
/// // The weak reference can be upgraded while strong exists
/// assert_eq!(*weak.upgrade().unwrap(), 42);
///
/// drop(strong);
/// rudo_gc::collect();
///
/// // After collection, upgrade returns None
/// assert!(weak.upgrade().is_none());
/// ```
pub struct Weak<T: Trace + 'static> {
    /// Pointer to the `GcBox`.
    /// Points to the allocation even after the value is dropped.
    ptr: AtomicNullable<GcBox<T>>,
}

impl<T: Trace> Weak<T> {
    /// Attempt to upgrade to a strong `Gc<T>` reference.
    ///
    /// Returns `None` if the value has been collected.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::{Gc, Weak};
    ///
    /// let gc = Gc::new(42);
    /// let weak = Gc::downgrade(&gc);
    ///
    /// assert!(weak.upgrade().is_some());
    /// ```
    /// Attempt to upgrade to a strong `Gc<T>` reference.
    ///
    /// Returns `None` if the value has been collected.
    ///
    /// # Panics
    ///
    /// Panics if the `Weak` points to a `GcBox` that is currently under construction
    /// (e.g., during `Gc::new_cyclic_weak`).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::{Gc, Weak};
    ///
    /// let gc = Gc::new(42);
    /// let weak = Gc::downgrade(&gc);
    ///
    /// assert!(weak.upgrade().is_some());
    /// ```
    pub fn upgrade(&self) -> Option<Gc<T>> {
        let ptr = self.ptr.load(Ordering::Acquire).as_option()?;

        unsafe {
            let gc_box = &*ptr.as_ptr();

            assert!(
                !gc_box.is_under_construction(),
                "Weak::upgrade: cannot upgrade while GcBox is under construction. \
                 This typically happens if you call upgrade() inside the closure \
                 passed to Gc::new_cyclic_weak()."
            );

            loop {
                if gc_box.is_value_dead() {
                    return None;
                }

                if gc_box.dropping_state() != 0 {
                    return None;
                }

                let current_count = gc_box.ref_count.load(Ordering::Relaxed);
                if current_count == 0 {
                    return None;
                }

                if current_count == usize::MAX {
                    return None;
                }

                if gc_box
                    .ref_count
                    .compare_exchange_weak(
                        current_count,
                        current_count.saturating_add(1),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    crate::gc::notify_created_gc();
                    return Some(Gc {
                        ptr: AtomicNullable::new(ptr),
                        _marker: PhantomData,
                    });
                }
            }
        }
    }

    /// Check if the referenced value is still alive.
    ///
    /// Returns `true` if the value can still be `upgrade()`d.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::{Gc, Weak};
    ///
    /// let gc = Gc::new(42);
    /// let weak = Gc::downgrade(&gc);
    ///
    /// assert!(weak.is_alive());
    ///
    /// drop(gc);
    /// rudo_gc::collect();
    ///
    /// assert!(!weak.is_alive());
    /// ```
    #[must_use]
    pub fn is_alive(&self) -> bool {
        let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
            return false;
        };

        // SAFETY: The pointer is valid because we have a weak reference
        unsafe { !(*ptr.as_ptr()).is_value_dead() }
    }

    /// Gets the number of strong `Gc<T>` pointers pointing to this allocation.
    ///
    /// Returns 0 if the value has been dropped.
    #[must_use]
    pub fn strong_count(&self) -> usize {
        let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
            return 0;
        };

        unsafe {
            if (*ptr.as_ptr()).is_value_dead() {
                0
            } else {
                (*ptr.as_ptr()).ref_count().get()
            }
        }
    }

    /// Gets the number of `Weak<T>` pointers pointing to this allocation.
    #[must_use]
    pub fn weak_count(&self) -> usize {
        let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
            return 0;
        };

        unsafe { (*ptr.as_ptr()).weak_count() }
    }

    /// Returns `true` if the two `Weak`s point to the same allocation.
    ///
    /// # Note
    ///
    /// Since a `Weak` reference does not own the value, the allocation
    /// may have been reclaimed. In that case, both `Weak`s may appear
    /// to point to different (invalid) memory.
    #[must_use]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.ptr.load(Ordering::Acquire) == other.ptr.load(Ordering::Acquire)
    }
}

impl<T: Trace> Clone for Weak<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Relaxed);
        if ptr.is_null() {
            return Self {
                ptr: AtomicNullable::null(),
            };
        }
        let gc_box_ptr = ptr.as_ptr();
        unsafe {
            (*gc_box_ptr).inc_weak();
        }
        Self {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
        }
    }
}

impl<T: Trace> Drop for Weak<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Relaxed);
        let Some(ptr) = ptr.as_option() else {
            return;
        };

        // SAFETY: Use raw pointer access to weak_count field directly.
        // This is critical for Stacked Borrows compliance: when Weak::drop is called
        // during the drop of a value inside a GcBox (e.g., a struct containing Weak<T>),
        // the GcBox's value field is under a mutable borrow from drop_in_place.
        // Creating a reference to the whole GcBox via (*ptr.as_ptr()).dec_weak() would
        // violate Stacked Borrows because it conflicts with the existing mutable borrow.
        // By using addr_of! to get the address of weak_count directly, we avoid
        // creating a reference to the GcBox and thus avoid the borrow conflict.
        unsafe {
            let weak_count_ptr = std::ptr::addr_of!((*ptr.as_ptr()).weak_count);

            let mut current = (*weak_count_ptr).load(Ordering::Relaxed);
            loop {
                let flags = current & GcBox::<T>::FLAGS_MASK;
                let count = current & !GcBox::<T>::FLAGS_MASK;

                match count.cmp(&1) {
                    std::cmp::Ordering::Equal => {
                        match (*weak_count_ptr).compare_exchange_weak(
                            current,
                            flags,
                            Ordering::AcqRel,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(actual) => current = actual,
                        }
                    }
                    std::cmp::Ordering::Greater => {
                        match (*weak_count_ptr).compare_exchange_weak(
                            current,
                            flags | (count - 1),
                            Ordering::AcqRel,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(actual) => current = actual,
                        }
                    }
                    std::cmp::Ordering::Less => break,
                }
            }
        }
    }
}

impl<T: Trace> std::fmt::Debug for Weak<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(Weak)")
    }
}

unsafe impl<T: Trace> Trace for Weak<T> {
    fn trace(&self, _visitor: &mut impl crate::trace::Visitor) {
        // Weak references do not need to be traced.
        // They don't keep the value alive, so tracing them would be incorrect.
    }
}

impl<T: Trace> Default for Weak<T> {
    /// Constructs a new `Weak<T>` that is dangling (cannot be upgraded).
    fn default() -> Self {
        Self {
            ptr: AtomicNullable::null(),
        }
    }
}

// ============================================================================
// Send + Sync trait implementations
// ============================================================================

#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + Send + Sync> Send for Gc<T> {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + Send + Sync> Sync for Gc<T> {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + Send + Sync> Send for Weak<T> {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + Send + Sync> Sync for Weak<T> {}

// ============================================================================
// Helper functions
// ============================================================================

/// Rehydrate dead self-references in a value.
fn rehydrate_self_refs<T: Trace>(_target: NonNull<GcBox<T>>, value: &T) {
    struct Rehydrator;

    impl Visitor for Rehydrator {
        fn visit<U: Trace>(&mut self, gc: &Gc<U>) {
            if gc.ptr.load(Ordering::Relaxed).is_null() {
                // FIXME: Self-referential cycle support is not implemented.
                //
                // Rehydration requires type information to ensure we only
                // rehydrate dead Gc<T> references that point to the same
                // allocation. Due to type erasure in our current design,
                // we cannot safely verify type compatibility here.
                //
                // Potential solutions:
                // 1. Store a unique allocation ID in GcBox for comparison
                // 2. Use runtime type information (RTTI)
                // 3. Require users to manually rehydrate after construction
                //
                // Until this is implemented, new_cyclic should be considered
                // non-functional and should not be used.
            }
        }

        unsafe fn visit_region(&mut self, _ptr: *const u8, _len: usize) {}
    }

    let mut rehydrator = Rehydrator;
    value.trace(&mut rehydrator);
}
