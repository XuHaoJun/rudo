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

/// Minimum valid heap address.
///
/// Addresses below this threshold are in the null page and are never valid
/// heap allocations. This value is based on the standard 4KB page size
/// used by most operating systems.
const MIN_VALID_HEAP_ADDRESS: usize = 4096;

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
    /// Per-object generation. Enables barrier early-exit when parent is young (no page load).
    /// Philosophy: check `weak_count` before touching `PageHeader`.
    pub(crate) const GEN_OLD_FLAG: usize = 1 << (usize::BITS - 3);
    /// Combined mask for all flags stored in `weak_count`.
    const FLAGS_MASK: usize = Self::DEAD_FLAG | Self::UNDER_CONSTRUCTION_FLAG | Self::GEN_OLD_FLAG;

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

    /// Try to increment `ref_count` atomically when it is currently zero.
    /// Returns true if successful, false if `ref_count` was non-zero or object is dead.
    ///
    /// This is used by weak upgrades to atomically transition from ref=0 to ref=1
    /// without racing with concurrent collection. The transition is only allowed
    /// if the object is fully alive (not under construction, not dead).
    ///
    /// # Safety
    ///
    /// Caller must ensure that if this returns true, they will properly use the
    /// resulting strong reference to prevent use-after-free.
    pub(crate) fn try_inc_ref_from_zero(&self) -> bool {
        loop {
            let ref_count = self.ref_count.load(Ordering::Acquire);
            let weak_count_raw = self.weak_count.load(Ordering::Acquire);

            let flags = weak_count_raw & Self::FLAGS_MASK;
            let weak_count = weak_count_raw & !Self::FLAGS_MASK;

            if flags != 0 && weak_count == 0 {
                return false;
            }

            if ref_count != 0 {
                return false;
            }

            match self
                .ref_count
                .compare_exchange_weak(0, 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(new_count) => {
                    if new_count != 0 {
                        return false;
                    }
                }
            }
        }
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

    /// Check if the `DEAD_FLAG` is set on this `GcBox`.
    ///
    /// The `DEAD_FLAG` indicates the value has been dropped but weak references
    /// may still exist. Use [`is_dead_or_unrooted()`] to check if the object
    /// is collectible (dead flag set OR no strong refs).
    pub fn has_dead_flag(&self) -> bool {
        (self.weak_count.load(Ordering::Relaxed) & Self::DEAD_FLAG) != 0
    }

    /// Mark the value as dropped.
    pub(crate) fn set_dead(&self) {
        self.weak_count.fetch_or(Self::DEAD_FLAG, Ordering::Relaxed);
    }

    /// Check if this `GcBox` is dead or unrooted (collectible).
    ///
    /// Returns true if the `DEAD_FLAG` is set OR if there are no strong references
    /// (`ref_count` == 0). An object is collectible when either condition holds.
    /// Use [`has_dead_flag()`] to check only the `DEAD_FLAG`.
    pub(crate) fn is_dead_or_unrooted(&self) -> bool {
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

    /// Set `GEN_OLD_FLAG` on promotion. Enables barrier early-exit for young objects.
    /// Uses `Release` ordering so barrier reads (Acquire) synchronize with promotion.
    #[inline]
    pub(crate) fn set_gen_old(&self) {
        self.weak_count
            .fetch_or(Self::GEN_OLD_FLAG, Ordering::Release);
    }

    /// Check if `GEN_OLD_FLAG` is set. For use in write barriers only.
    /// Uses `Acquire` ordering to synchronize with `set_gen_old` (Release).
    #[inline]
    pub(crate) fn has_gen_old_flag(&self) -> bool {
        (self.weak_count.load(Ordering::Acquire) & Self::GEN_OLD_FLAG) != 0
    }

    /// Clear `GEN_OLD_FLAG`. Used when deallocating so reused slots don't inherit stale state.
    #[inline]
    pub(crate) fn clear_gen_old(&self) {
        self.weak_count
            .fetch_and(!Self::GEN_OLD_FLAG, Ordering::Relaxed);
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
        // Increment the weak count to track this weak reference.
        // SAFETY: self is a valid GcBox pointer.
        unsafe {
            (*NonNull::from(self).as_ptr()).inc_weak();
        }
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
    ///
    /// Uses atomic CAS to transition from `ref_count=0` to `ref_count=1`,
    /// preventing races with concurrent GC that could resurrect dead objects.
    /// If `ref_count` > 0, increments normally since strong refs already exist.
    pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
        let ptr = self.ptr.load(Ordering::Acquire).as_option()?;

        unsafe {
            let gc_box = &*ptr.as_ptr();

            if gc_box.is_under_construction() {
                return None;
            }

            // If DEAD_FLAG is set, value has been dropped - cannot resurrect
            if gc_box.has_dead_flag() {
                return None;
            }

            // Try atomic transition from 0 to 1 (resurrection)
            if gc_box.try_inc_ref_from_zero() {
                return Some(Gc {
                    ptr: AtomicNullable::new(ptr),
                    _marker: PhantomData,
                });
            }

            gc_box.inc_ref();
            Some(Gc {
                ptr: AtomicNullable::new(ptr),
                _marker: PhantomData,
            })
        }
    }

    /// Clone the weak reference.
    pub(crate) fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire).as_option().unwrap();
        unsafe {
            (*ptr.as_ptr()).inc_weak();
        }
        Self {
            ptr: AtomicNullable::new(ptr),
        }
    }

    /// Get the raw pointer, for use in Drop implementations.
    pub(crate) fn as_ptr(&self) -> Option<NonNull<GcBox<T>>> {
        self.ptr.load(Ordering::Acquire).as_option()
    }

    /// Check if this weak reference might be valid (lightweight check).
    pub(crate) fn may_be_valid(&self) -> bool {
        let ptr = self.ptr.load(Ordering::Acquire);

        if ptr.is_null() {
            return false;
        }

        let Some(ptr) = ptr.as_option() else {
            return false;
        };

        let addr = ptr.as_ptr() as usize;
        let alignment = std::mem::align_of::<GcBox<T>>();
        addr >= 4096 && addr % alignment == 0
    }

    /// Attempt upgrade with additional safety checks.
    pub(crate) fn try_upgrade(&self) -> Option<Gc<T>> {
        let ptr = self.ptr.load(Ordering::Acquire);

        let ptr = ptr.as_option()?;

        let addr = ptr.as_ptr() as usize;
        let alignment = std::mem::align_of::<GcBox<T>>();
        if addr % alignment != 0 || addr < MIN_VALID_HEAP_ADDRESS {
            return None;
        }

        unsafe {
            // SAFETY: Pointer passed validation checks above (properly aligned, addr >= 4096)
            let gc_box = &*ptr.as_ptr();

            if gc_box.is_under_construction() {
                return None;
            }

            if gc_box.is_dead_or_unrooted() {
                return None;
            }

            if gc_box.dropping_state() != 0 {
                return None;
            }

            // Try atomic transition from 0 to 1 (same as regular upgrade)
            if gc_box.try_inc_ref_from_zero() {
                crate::gc::notify_created_gc();
                return Some(Gc {
                    ptr: AtomicNullable::new(ptr),
                    _marker: PhantomData,
                });
            }

            // ref_count > 0, check again if still alive
            if gc_box.is_dead_or_unrooted() {
                return None;
            }

            // Check for overflow (for consistency with public Weak<T>::try_upgrade)
            let current_count = gc_box.ref_count.load(Ordering::Acquire);
            if current_count == usize::MAX {
                return None;
            }

            // Object is alive and has strong refs - increment normally
            gc_box.inc_ref();
            Some(Gc {
                ptr: AtomicNullable::new(ptr),
                _marker: PhantomData,
            })
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

        // Record for suspicious sweep detection
        #[cfg(feature = "debug-suspicious-sweep")]
        crate::gc::record_young_object(ptr.as_ptr() as *const u8);

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

        // Record for suspicious sweep detection
        #[cfg(feature = "debug-suspicious-sweep")]
        crate::gc::record_young_object(gc_box_ptr.as_ptr() as *const u8);

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

        // Record for suspicious sweep detection
        #[cfg(feature = "debug-suspicious-sweep")]
        crate::gc::record_young_object(gc_box_ptr.as_ptr() as *const u8);

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
            return None;
        }
        let gc_box_ptr = ptr.as_ptr();
        unsafe {
            if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
                return None;
            }
            Some(&(*gc_box_ptr).value)
        }
    }

    /// Attempt to clone this `Gc`.
    ///
    /// Returns `None` if this Gc is "dead".
    pub fn try_clone(gc: &Self) -> Option<Self> {
        let ptr = gc.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return None;
        }
        let gc_box_ptr = ptr.as_ptr();
        unsafe {
            if (*gc_box_ptr).has_dead_flag() {
                return None;
            }
        }
        Some(gc.clone())
    }

    /// Get a raw pointer to the data.
    ///
    /// # Safety
    ///
    /// The caller is responsible for ensuring that the `Gc` is still alive
    /// (i.e. not dead or in dropping state) before dereferencing the returned
    /// pointer. Dereferencing a pointer obtained from a dead `Gc` is undefined
    /// behaviour. Use [`Gc::try_deref`] for a safe alternative.
    pub fn as_ptr(&self) -> *const T {
        let ptr = self.ptr.load(Ordering::Acquire);
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
    pub fn is_dead_or_unrooted(gc: &Self) -> bool {
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
        unsafe { (*ptr.as_ptr()).inc_ref() };

        drop(roots);

        GcHandle {
            ptr,
            origin_tcb: Arc::downgrade(&tcb),
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
        unsafe {
            (*self.as_non_null().as_ptr()).inc_weak();
        }
        crate::handles::WeakCrossThreadHandle {
            weak: GcBoxWeakRef::new(self.as_non_null()),
            origin_tcb: std::sync::Arc::downgrade(
                &crate::heap::current_thread_control_block()
                    .expect("weak_cross_thread_handle called outside of GC context"),
            ),
            origin_thread: std::thread::current().id(),
        }
    }
}

impl<T: Trace> Deref for Gc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        let ptr = self.ptr.load(Ordering::Acquire);
        let gc_box_ptr = ptr.as_ptr();
        unsafe {
            assert!(
                !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
                "Gc::deref: cannot dereference a dead Gc"
            );
            &(*gc_box_ptr).value
        }
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
                if gc_box.has_dead_flag() {
                    return None;
                }

                if gc_box.dropping_state() != 0 {
                    return None;
                }

                let current_count = gc_box.ref_count.load(Ordering::Acquire);
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

    /// Attempt to upgrade to a strong reference with additional safety checks.
    ///
    /// Returns `None` if:
    /// - The weak ref is null
    /// - The object has been collected (`DEAD_FLAG` set)
    /// - The reference count is 0
    /// - The memory location is obviously invalid (misaligned or too low address)
    ///
    /// This method performs additional validation beyond the standard upgrade
    /// to ensure memory safety when the weak ref's validity is uncertain.
    ///
    /// # Use Cases
    ///
    /// Use this method when weak references are stored in data structures
    /// that may contain corrupted or stale pointers (e.g., reactive signal
    /// subscriber lists where effects may be recreated).
    #[inline]
    pub fn try_upgrade(&self) -> Option<Gc<T>> {
        let ptr = self.ptr.load(Ordering::Acquire);

        let ptr = ptr.as_option()?;

        let addr = ptr.as_ptr() as usize;

        let alignment = std::mem::align_of::<GcBox<T>>();
        if addr % alignment != 0 {
            return None;
        }

        if addr < MIN_VALID_HEAP_ADDRESS {
            return None;
        }
        if !is_gc_box_pointer_valid(addr) {
            return None;
        }

        unsafe {
            // SAFETY: Pointer passed validation checks above (alignment, addr >= 4096)
            let gc_box = &*ptr.as_ptr();

            if gc_box.is_under_construction() {
                return None;
            }

            loop {
                if gc_box.has_dead_flag() {
                    return None;
                }

                if gc_box.dropping_state() != 0 {
                    return None;
                }

                let current_count = gc_box.ref_count.load(Ordering::Acquire);
                if current_count == 0 || current_count == usize::MAX {
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

    /// Check if this weak reference might be valid.
    ///
    /// This is a lightweight check that doesn't require dereferencing.
    /// Returns `false` if the weak ref is definitely invalid.
    /// Returns `true` if it might be valid (needs `try_upgrade` to confirm).
    ///
    /// Use this for pre-filtering before calling `try_upgrade` in tight loops.
    #[inline]
    #[must_use]
    pub fn may_be_valid(&self) -> bool {
        let ptr = self.ptr.load(Ordering::Acquire);

        if ptr.is_null() {
            return false;
        }

        let Some(ptr) = ptr.as_option() else {
            return false;
        };

        let addr = ptr.as_ptr() as usize;

        let alignment = std::mem::align_of::<GcBox<T>>();
        addr >= 4096 && addr % alignment == 0
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
        // Delegate to upgrade() to avoid TOCTOU: between loading ptr and checking
        // has_dead_flag(), GC could reclaim the object. upgrade() uses atomic
        // compare_exchange to safely acquire a strong ref when the object is alive.
        self.upgrade().is_some()
    }

    /// Casts this `Weak<T>` to a `Weak<U>`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `T` and `U` have the same layout and that
    /// the cast is valid for the lifetime of the pointer.
    pub fn cast<U: Trace + 'static>(self) -> Weak<U> {
        let ptr = self.ptr.load(Ordering::Acquire);
        std::mem::forget(self);
        let atomic_ptr = ptr.as_option().map_or_else(AtomicNullable::null, |p| {
            let cast_p: NonNull<GcBox<U>> = unsafe { std::mem::transmute(p) };
            AtomicNullable::new(cast_p)
        });
        Weak { ptr: atomic_ptr }
    }
    /// Gets the number of strong `Gc<T>` pointers pointing to this allocation.
    ///
    /// Returns 0 if the value has been dropped.
    #[must_use]
    pub fn strong_count(&self) -> usize {
        let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
            return 0;
        };
        let ptr_addr = ptr.as_ptr() as usize;
        let alignment = std::mem::align_of::<GcBox<T>>();
        if ptr_addr % alignment != 0 {
            return 0;
        }

        unsafe {
            if (*ptr.as_ptr()).has_dead_flag() {
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

    /// Returns the raw gc-box address stored by this weak pointer.
    #[must_use]
    pub fn raw_addr(&self) -> usize {
        self.ptr
            .load(Ordering::Acquire)
            .as_option()
            .map_or(0, |p| p.as_ptr() as usize)
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
        let ptr_addr = ptr.as_ptr() as usize;
        let alignment = std::mem::align_of::<GcBox<T>>();
        if ptr_addr % alignment != 0 {
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

#[inline]
fn is_gc_box_pointer_valid(ptr_addr: usize) -> bool {
    let ptr = ptr_addr as *const u8;

    // Fast path: current thread heap.
    if crate::heap::try_with_heap(|heap| unsafe {
        crate::heap::find_gc_box_from_ptr(heap, ptr).is_some()
    })
    .unwrap_or(false)
    {
        return true;
    }

    // Cross-thread path: scan all registered heaps.
    for tcb in crate::heap::get_all_thread_control_blocks() {
        // SAFETY: We only do read-only heap metadata checks here.
        let heap = unsafe { &*tcb.heap.get() };
        if unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr).is_some() } {
            return true;
        }
    }

    false
}

impl<T: Trace> Drop for Weak<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Relaxed);
        let Some(ptr) = ptr.as_option() else {
            return;
        };

        let ptr_addr = ptr.as_ptr() as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {
            self.ptr.set_null();
            return;
        }

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
// `Ephemeron<K, V>` - Key-value pair where value is only reachable if key is
// ============================================================================

/// An ephemeron is a key-value pair where the value is only reachable if the key is reachable.
///
/// Unlike `Weak<T>` which only holds a weak reference to a single value, an `Ephemeron<K, V>`:
/// - Holds a weak reference to the key
/// - Holds a strong reference to the value
/// - The value can only be accessed (via `upgrade`) if the key is still alive from roots
/// - When the key becomes unreachable from roots, the value becomes unreachable too
///
/// This is useful for implementing weak caches, memoization tables, and other
/// data structures where the value should be collected when the key is no longer
/// reachable from roots.
///
/// # Example
///
/// ```ignore
/// use rudo_gc::{Gc, Ephemeron, collect_full};
///
/// let key = Gc::new("key");
/// let value = Gc::new(42);
/// let ephemeron = Ephemeron::new(&key, value);
///
/// // Value is accessible because key is alive
/// assert!(ephemeron.upgrade().is_some());
///
/// // Drop the key root
/// drop(key);
/// collect_full();
///
/// // Value is no longer accessible because key is dead
/// assert!(ephemeron.upgrade().is_none());
/// ```
///
/// # Current Implementation Status
///
/// **Partial Implementation**: This type provides the correct API for ephemeron semantics,
/// but full GC-level semantics are not yet implemented.
///
/// ✅ What works:
/// - `upgrade()` correctly returns `None` when the key is no longer reachable
/// - `is_key_alive()` correctly detects when key has been collected
/// - The value is properly kept alive while the key is alive
///
/// ⚠️ Current limitation:
/// - The value is NOT automatically collected when the key becomes unreachable
/// - This is because the current `Trace` implementation unconditionally traces the value
/// - Full implementation would require GC-level ephemeron tracking (future work)
///
/// This limitation means memory usage may be higher than expected for some use cases,
/// but the API is sound and correctly prevents accessing values when keys are dead.
///
/// # Future Work
///
/// Full ephemeron semantics would require:
/// 1. Track ephemerons in a global list during marking
/// 2. In mark phase: if key is marked, trace value; if not, skip (broken ephemeron)
/// 3. In sweep phase: clear broken ephemerons' value references
pub struct Ephemeron<K: Trace + 'static, V: Trace + 'static> {
    /// Weak reference to key - does NOT keep key alive
    key: Weak<K>,
    /// Strong reference to value - keeps value alive IF key is alive
    value: Gc<V>,
}

impl<K: Trace + 'static, V: Trace + 'static> Ephemeron<K, V> {
    /// Creates a new ephemeron from a key and value.
    ///
    /// The key is stored as a weak reference (does not keep key alive).
    /// The value is stored as a strong reference (keeps value alive).
    /// The value will only be accessible via `upgrade()` while the key is still reachable.
    ///
    /// # Important
    ///
    /// The `key` parameter is taken by reference to prevent it from being dropped
    /// when passed to this function. The caller must ensure the key remains alive
    /// for the duration of this call (which is guaranteed by passing a reference).
    pub fn new(key: &Gc<K>, value: Gc<V>) -> Self {
        let weak_key = Gc::downgrade(key);
        Self {
            key: weak_key,
            value,
        }
    }

    /// Returns a reference to the key (if still alive).
    ///
    /// Returns `None` if the key has been collected.
    pub const fn key(&self) -> Option<&Gc<K>> {
        // This is tricky - Weak doesn't give us &Gc<K>, it gives us Option<Gc<K>>
        // For now, we don't expose direct key access
        None
    }

    /// Returns a reference to the value without checking if the key is alive.
    ///
    /// # Safety
    ///
    /// The caller must ensure the key is alive before accessing the value.
    /// Use `upgrade()` for safe access to the value.
    pub const unsafe fn value_unsafe(&self) -> &Gc<V> {
        &self.value
    }

    /// Attempts to upgrade to a strong reference to the value.
    ///
    /// Returns `Some(Gc<V>)` if the key is still alive and reachable.
    /// Returns `None` if the key has been collected (or was never rooted).
    ///
    /// This is the primary way to access the value - it ensures type safety
    /// by only returning the value when the key is still alive.
    pub fn upgrade(&self) -> Option<Gc<V>> {
        if self.is_key_alive() {
            // Clone the Gc to return - this increments the ref count
            Gc::try_clone(&self.value)
        } else {
            None
        }
    }

    /// Checks if the key is still alive and reachable.
    ///
    /// Returns `true` if the key has not been collected.
    /// Returns `false` if the key has been collected.
    pub fn is_key_alive(&self) -> bool {
        self.key.is_alive()
    }

    /// Checks if the ephemeron might be valid (lightweight check).
    ///
    /// This is a fast check that doesn't fully validate the key or value.
    /// Use `is_key_alive()` for a more thorough check.
    pub fn may_be_valid(&self) -> bool {
        self.key.may_be_valid()
    }
}

impl<K: Trace + 'static, V: Trace + 'static> Clone for Ephemeron<K, V> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            value: Gc::try_clone(&self.value).unwrap_or_else(|| Gc {
                ptr: AtomicNullable::null(),
                _marker: PhantomData,
            }),
        }
    }
}

impl<K: Trace + 'static, V: Trace + 'static> Default for Ephemeron<K, V> {
    fn default() -> Self {
        Self {
            key: Weak {
                ptr: AtomicNullable::null(),
            },
            value: Gc {
                ptr: AtomicNullable::null(),
                _marker: PhantomData,
            },
        }
    }
}

impl<K: Trace + 'static, V: Trace + 'static> std::fmt::Debug for Ephemeron<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ephemeron")
            .field("key_alive", &self.is_key_alive())
            .field("value_accessible", &self.upgrade().is_some())
            .finish()
    }
}

unsafe impl<K: Trace + 'static, V: Trace + 'static> Trace for Ephemeron<K, V> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // For basic ephemeron implementation:
        // - The key is stored as a Weak, so it's not traced (correct)
        // - The value is always traced while the ephemeron exists
        //
        // NOTE: This keeps the value alive as long as the ephemeron exists.
        // For true ephemeron semantics where value is collected when key dies,
        // the GC would need special ephemeron handling:
        // - Track ephemerons in a special list during marking
        // - Only trace value if key was marked (reachable)
        // - Clear broken ephemerons during sweep
        //
        // For now, this basic implementation provides the API but not the
        // full GC semantics. The value will stay in memory even after key dies.
        visitor.visit(&self.value);
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
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<K: Trace + Send + Sync, V: Trace + Send + Sync> Send for Ephemeron<K, V> {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<K: Trace + Send + Sync, V: Trace + Send + Sync> Sync for Ephemeron<K, V> {}

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
