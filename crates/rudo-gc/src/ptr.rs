//! The `Gc<T>` smart pointer implementation.
//!
//! This module provides the primary user-facing type for garbage-collected
//! memory management.

use std::cell::Cell;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::gc::{is_collecting, notify_dropped_gc};
use crate::heap::{ptr_to_object_index, ptr_to_page_header, with_heap, LocalHeap};
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
    pub fn ref_count(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.ref_count.load(Ordering::Relaxed))
            .expect("ref_count should never be zero for live GcBox")
    }

    /// Check if the value is currently under construction.
    #[inline]
    fn is_under_construction(&self) -> bool {
        (self.weak_count.load(Ordering::Relaxed) & Self::UNDER_CONSTRUCTION_FLAG) != 0
    }

    /// Set the under-construction flag.
    #[inline]
    fn set_under_construction(&self, flag: bool) {
        let mask = Self::UNDER_CONSTRUCTION_FLAG;
        if flag {
            self.weak_count.fetch_or(mask, Ordering::Relaxed);
        } else {
            self.weak_count.fetch_and(!mask, Ordering::Relaxed);
        }
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
            let count = this.ref_count.load(Ordering::Relaxed);
            if count == 0 {
                // Already at zero - this is a bug (double-free or use-after-free)
                // Return true to prevent further issues
                return true;
            }
            if count == 1 {
                // Last reference - drop the value
                // SAFETY: We're the last reference, safe to drop
                // The drop function handles value dropping
                unsafe {
                    (this.drop_fn)(self_ptr.cast::<u8>());
                }
                return true;
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
}

impl<T: Trace> GcBox<T> {
    /// Type-erased drop function for any Sized T.
    pub(crate) unsafe fn drop_fn_for(ptr: *mut u8) {
        // SAFETY: The caller must ensure ptr points to a GcBox<T> where T: Sized.
        // This is true for all objects allocated via Gc::new.
        let gc_box = ptr.cast::<Self>();
        unsafe {
            std::ptr::drop_in_place(std::ptr::addr_of_mut!((*gc_box).value));
            // Mark as dropped to avoid double-dropping during sweep
            (*gc_box).drop_fn = GcBox::<()>::no_op_drop;
            (*gc_box).trace_fn = GcBox::<()>::no_op_trace;
            (*gc_box).set_dead();
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
}

impl GcBox<()> {
    /// A no-op drop function for already-dropped objects.
    pub(crate) const unsafe fn no_op_drop(_ptr: *mut u8) {}

    /// A no-op trace function for already-dropped objects.
    pub(crate) const unsafe fn no_op_trace(_ptr: *const u8, _visitor: &mut GcVisitor) {}
}

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
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

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
    fn new_zst(value: T) -> Self {
        debug_assert!(std::mem::size_of::<T>() == 0);

        // For ZSTs, we use a special sentinel address that's:
        // 1. Non-null (so we can distinguish from dead Gc)
        // 2. Aligned for GcBox<T>
        // 3. Never actually dereferenced for its value
        //
        // We allocate a minimal GcBox to hold the ZST ref count.
        // Since the value is zero-sized, this is just the ref_count field.

        // Use thread-local singleton for ZST
        thread_local! {
            static ZST_BOX: Cell<Option<NonNull<u8>>> = const { Cell::new(None) };
        }

        let gc_box_ptr = ZST_BOX.with(|cell| {
            cell.get().map_or_else(
                || {
                    // First ZST allocation - create the singleton
                    let ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
                    let gc_box = ptr.as_ptr().cast::<GcBox<T>>();

                    // SAFETY: We just allocated this memory
                    unsafe {
                        gc_box.write(GcBox {
                            ref_count: AtomicUsize::new(1),
                            weak_count: AtomicUsize::new(0),
                            drop_fn: GcBox::<T>::drop_fn_for,
                            trace_fn: GcBox::<T>::trace_fn_for,
                            value,
                        });
                    }

                    cell.set(Some(ptr));

                    unsafe { NonNull::new_unchecked(gc_box) }
                },
                |ptr| {
                    // Reuse existing ZST allocation
                    // Increment ref count
                    let gc_box = ptr.as_ptr().cast::<GcBox<T>>();
                    // SAFETY: We know this is a valid GcBox<T> for ZST
                    unsafe {
                        (*gc_box).inc_ref();
                    }
                    unsafe { NonNull::new_unchecked(gc_box) }
                },
            )
        });

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

        struct DropGuard {
            ptr: NonNull<u8>,
            completed: bool,
        }

        impl Drop for DropGuard {
            fn drop(&mut self) {
                if !self.completed {
                    with_heap(|heap| unsafe {
                        heap.dealloc(self.ptr);
                    });
                }
            }
        }

        let raw_ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
        let gc_box = raw_ptr.as_ptr().cast::<GcBox<T>>();

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        let mut guard = DropGuard {
            ptr: raw_ptr,
            completed: false,
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

        if is_collecting() {
            unsafe {
                let header = ptr_to_page_header(gc_box_ptr.cast());
                if (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE {
                    if let Some(index) = ptr_to_object_index(gc_box_ptr.cast()) {
                        if !(*header.as_ptr()).is_marked(index) {
                            return;
                        }
                    }
                }
            }
        }

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

        // SAFETY: The pointer is valid because we have a weak reference
        unsafe {
            // Check if under construction (prevents UB during new_cyclic_weak)
            assert!(
                !(*ptr.as_ptr()).is_under_construction(),
                "Weak::upgrade: cannot upgrade while GcBox is under construction. \
                 This typically happens if you call upgrade() inside the closure \
                 passed to Gc::new_cyclic_weak()."
            );

            // Check if the value is still alive
            if (*ptr.as_ptr()).is_value_dead() {
                return None;
            }

            // Increment the strong reference count
            (*ptr.as_ptr()).inc_ref();

            // Notify the GC about the new Gc
            crate::gc::notify_created_gc();

            Some(Gc {
                ptr: AtomicNullable::new(ptr),
                _marker: PhantomData,
            })
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

            // Load current value atomically for proper synchronization
            let current = (*weak_count_ptr).load(Ordering::Relaxed);
            let flags = current & GcBox::<T>::FLAGS_MASK;
            let count = current & !GcBox::<T>::FLAGS_MASK;

            // Decrement the weak count, preserving flags
            if count > 1 {
                (*weak_count_ptr).store(flags | (count - 1), Ordering::Relaxed);
            } else if count == 1 {
                (*weak_count_ptr).store(flags, Ordering::Relaxed);
            }
            // If count == 0, nothing to do (already at zero)
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
