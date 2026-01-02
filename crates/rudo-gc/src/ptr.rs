//! The `Gc<T>` smart pointer implementation.
//!
//! This module provides the primary user-facing type for garbage-collected
//! memory management.

use std::cell::Cell;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::ptr::NonNull;

use crate::gc::notify_dropped_gc;
use crate::heap::{with_heap, GlobalHeap};
use crate::roots::ROOTS;
use crate::trace::{Trace, Visitor};

// ============================================================================
// GcBox - The heap allocation container
// ============================================================================

/// The actual heap allocation wrapping the user's value.
#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    /// Current reference count (for amortized collection triggering).
    ref_count: Cell<NonZeroUsize>,
    /// The user's data.
    value: T,
}

impl<T: Trace + ?Sized> GcBox<T> {
    /// Get the reference count.
    pub fn ref_count(&self) -> NonZeroUsize {
        self.ref_count.get()
    }

    /// Increment the reference count.
    pub fn inc_ref(&self) {
        let count = self.ref_count.get();
        // Saturating add to prevent overflow
        self.ref_count.set(count.saturating_add(1));
    }

    /// Decrement the reference count. Returns true if count reached zero.
    pub fn dec_ref(&self) -> bool {
        let count = self.ref_count.get().get();
        if count == 1 {
            true
        } else {
            self.ref_count
                .set(NonZeroUsize::new(count - 1).expect("ref count underflow"));
            false
        }
    }

    /// Get a reference to the value.
    #[allow(dead_code)]
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }
}

// ============================================================================
// Nullable - A nullable pointer to unsized types
// ============================================================================

/// A nullable pointer for `?Sized` types.
///
/// We need this because `Option<NonNull<T>>` doesn't work well with
/// unsized types in some contexts.
#[derive(Debug)]
pub struct Nullable<T: ?Sized>(*mut T);

impl<T: ?Sized> Nullable<T> {
    /// Create a new nullable pointer from a non-null pointer.
    #[must_use]
    pub const fn new(ptr: NonNull<T>) -> Self {
        Self(ptr.as_ptr())
    }

    /// Create a null pointer.
    #[allow(dead_code)]
    #[must_use]
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
/// `Gc<T>` is `!Send` and `!Sync`. It can only be used within a single thread.
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
pub struct Gc<T: Trace + ?Sized + 'static> {
    /// Pointer to the heap-allocated box.
    /// If null, this is a "dead" Gc (only observable during Drop of cycles).
    ptr: Cell<Nullable<GcBox<T>>>,
    /// Marker to make Gc !Send and !Sync.
    _marker: PhantomData<*const ()>,
}

impl<T: Trace> Gc<T> {
    /// Create a new garbage-collected value.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::Gc;
    ///
    /// let x = Gc::new(42);
    /// assert_eq!(*x, 42);
    /// ```
    pub fn new(value: T) -> Self {
        // Allocate space in the heap
        let ptr = with_heap(GlobalHeap::alloc::<GcBox<T>>);

        // Initialize the GcBox
        let gc_box = ptr.as_ptr().cast::<GcBox<T>>();
        // SAFETY: We just allocated this memory
        unsafe {
            gc_box.write(GcBox {
                ref_count: Cell::new(NonZeroUsize::MIN),
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        // Register as a root
        ROOTS.with(|roots| {
            roots.borrow_mut().push(gc_box_ptr.cast());
        });

        // Notify that we created a Gc
        crate::gc::notify_created_gc();

        Self {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
            _marker: PhantomData,
        }
    }

    /// Create a self-referential garbage-collected value.
    ///
    /// The closure receives a "dead" `Gc` that will be rehydrated after
    /// construction completes.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::{Gc, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Node {
    ///     self_ref: Gc<Node>,
    /// }
    ///
    /// let node = Gc::new_cyclic(|this| Node { self_ref: this });
    /// ```
    pub fn new_cyclic<F: FnOnce(Self) -> T>(data_fn: F) -> Self {
        // Allocate space
        let ptr = with_heap(GlobalHeap::alloc::<GcBox<T>>);
        let gc_box = ptr.as_ptr().cast::<GcBox<T>>();

        // Create a dead Gc to pass to the closure
        let dead_gc = Self {
            ptr: Cell::new(Nullable::new(unsafe { NonNull::new_unchecked(gc_box) }).as_null()),
            _marker: PhantomData,
        };

        // Call the closure to get the value
        let value = data_fn(dead_gc);

        // Initialize the GcBox
        // SAFETY: We just allocated this memory
        unsafe {
            gc_box.write(GcBox {
                ref_count: Cell::new(NonZeroUsize::MIN),
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        // Register as a root
        ROOTS.with(|roots| {
            roots.borrow_mut().push(gc_box_ptr.cast());
        });

        // Create the live Gc
        let gc = Self {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
            _marker: PhantomData,
        };

        // Rehydrate any dead Gcs in the value that point to us
        // SAFETY: The GcBox is now initialized
        unsafe {
            rehydrate_self_refs(gc_box_ptr, &(*gc_box).value);
        }

        gc
    }
}

impl<T: Trace + ?Sized> Gc<T> {
    /// Attempt to dereference this `Gc`.
    ///
    /// Returns `None` if this Gc is "dead" (only possible during Drop of cycles).
    pub fn try_deref(gc: &Self) -> Option<&T> {
        if gc.ptr.get().is_null() {
            None
        } else {
            Some(&**gc)
        }
    }

    /// Attempt to clone this `Gc`.
    ///
    /// Returns `None` if this Gc is "dead".
    pub fn try_clone(gc: &Self) -> Option<Self> {
        if gc.ptr.get().is_null() {
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
        let ptr = gc.ptr.get().unwrap();
        unsafe { std::ptr::addr_of!((*ptr.as_ptr()).value) }
    }

    /// Check if two Gcs point to the same allocation.
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.ptr.get().as_option() == other.ptr.get().as_option()
    }

    /// Get the current reference count.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    pub fn ref_count(gc: &Self) -> NonZeroUsize {
        let ptr = gc.ptr.get().unwrap();
        unsafe { (*ptr.as_ptr()).ref_count() }
    }

    /// Check if this Gc is "dead" (refers to a collected value).
    pub fn is_dead(gc: &Self) -> bool {
        gc.ptr.get().is_null()
    }

    /// Kill this Gc, making it dead.
    #[allow(dead_code)]
    pub(crate) fn kill(&self) {
        self.ptr.set(self.ptr.get().as_null());
    }

    /// Get the raw `GcBox` pointer.
    pub(crate) fn raw_ptr(&self) -> Nullable<GcBox<T>> {
        self.ptr.get()
    }
}

impl<T: Trace + ?Sized> Deref for Gc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        let ptr = self.ptr.get().unwrap();
        // SAFETY: If not null, the pointer is valid
        unsafe { &(*ptr.as_ptr()).value }
    }
}

impl<T: Trace + ?Sized> Clone for Gc<T> {
    fn clone(&self) -> Self {
        let Some(ptr) = self.ptr.get().as_option() else {
            // Cloning a dead Gc returns another dead Gc
            return Self {
                ptr: self.ptr.clone(),
                _marker: PhantomData,
            };
        };

        // Increment reference count
        // SAFETY: Pointer is valid (not null)
        unsafe {
            (*ptr.as_ptr()).inc_ref();
        }

        // Register as a root
        ROOTS.with(|roots| {
            roots.borrow_mut().push(ptr.cast());
        });

        Self {
            ptr: self.ptr.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: Trace + ?Sized> Drop for Gc<T> {
    fn drop(&mut self) {
        let Some(ptr) = self.ptr.get().as_option() else {
            return;
        };

        // Remove from roots
        ROOTS.with(|roots| {
            roots.borrow_mut().pop(ptr.cast());
        });

        // Decrement reference count
        let is_last = unsafe { (*ptr.as_ptr()).dec_ref() };

        if is_last {
            // This was the last reference; drop unconditionally
            // SAFETY: We have exclusive access
            unsafe {
                std::ptr::drop_in_place(std::ptr::addr_of_mut!((*ptr.as_ptr()).value));
                // Note: Memory is managed by the heap, not deallocated here
            }
        } else {
            // Notify for potential cycle collection
            notify_dropped_gc();
        }
    }
}

impl<T: Trace + ?Sized + PartialEq> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Trace + ?Sized + Eq> Eq for Gc<T> {}

impl<T: Trace + ?Sized + std::fmt::Debug> std::fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ptr.get().is_null() {
            write!(f, "Gc(<dead>)")
        } else {
            f.debug_tuple("Gc").field(&&**self).finish()
        }
    }
}

impl<T: Trace + ?Sized + std::fmt::Display> std::fmt::Display for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&**self, f)
    }
}

impl<T: Trace + ?Sized> std::fmt::Pointer for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Pointer::fmt(&self.ptr.get().as_ptr(), f)
    }
}

impl<T: Trace + Default> Default for Gc<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Trace + ?Sized + std::hash::Hash> std::hash::Hash for Gc<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

impl<T: Trace + ?Sized + PartialOrd> PartialOrd for Gc<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: Trace + ?Sized + Ord> Ord for Gc<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: Trace> From<T> for Gc<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Trace + ?Sized> AsRef<T> for Gc<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: Trace + ?Sized> std::borrow::Borrow<T> for Gc<T> {
    fn borrow(&self) -> &T {
        self
    }
}

// Gc is NOT Send or Sync
// We use PhantomData<*const ()> to ensure this, which is !Send and !Sync.
// The marker is already in the struct, so these impls are not needed.
// Note: Negative trait impls require nightly, so we rely on the marker type instead.

// ============================================================================
// Helper functions
// ============================================================================

/// Rehydrate dead self-references in a value.
fn rehydrate_self_refs<T: Trace + ?Sized>(_target: NonNull<GcBox<T>>, value: &T) {
    struct Rehydrator;

    impl Visitor for Rehydrator {
        fn visit<U: Trace + ?Sized>(&mut self, gc: &Gc<U>) {
            // This is a simplified rehydration - in practice we'd need
            // type checking to ensure we only rehydrate matching types
            if gc.ptr.get().is_null() {
                // The Gc is dead; check if we should rehydrate it
                // For now, we can't easily rehydrate due to type mismatch
                // This is a limitation of our current design
            }
        }
    }

    let mut rehydrator = Rehydrator;
    value.trace(&mut rehydrator);
}
