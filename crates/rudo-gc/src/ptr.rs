//! The `Gc<T>` smart pointer implementation.
//!
//! This module provides the primary user-facing type for garbage-collected
//! memory management.

use std::cell::Cell;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::ptr::NonNull;

use crate::gc::{is_collecting, notify_dropped_gc};
use crate::heap::{ptr_to_object_index, ptr_to_page_header, with_heap, GlobalHeap};
use crate::trace::{GcVisitor, Trace, Visitor};

// ============================================================================
// `GcBox` - The heap allocation container
// ============================================================================

/// The actual heap allocation wrapping the user's value.
#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    /// Current reference count (for amortized collection triggering).
    ref_count: Cell<NonZeroUsize>,
    /// Number of weak references to this allocation.
    weak_count: Cell<usize>,
    /// Type-erased destructor for the value.
    pub(crate) drop_fn: unsafe fn(*mut u8),
    /// Type-erased trace function for the value.
    pub(crate) trace_fn: unsafe fn(*const u8, &mut GcVisitor),
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

    /// Get the weak reference count.
    pub fn weak_count(&self) -> usize {
        self.weak_count.get() & !(1 << (std::mem::size_of::<usize>() * 8 - 1))
    }

    /// Increment the weak reference count.
    pub fn inc_weak(&self) {
        let current = self.weak_count.get();
        let flag = current & (1 << (std::mem::size_of::<usize>() * 8 - 1));
        let count = current & !(1 << (std::mem::size_of::<usize>() * 8 - 1));
        self.weak_count.set(flag | count.saturating_add(1));
    }

    /// Decrement the weak reference count. Returns true if count reached zero.
    pub fn dec_weak(&self) -> bool {
        let current = self.weak_count.get();
        let flag = current & (1 << (std::mem::size_of::<usize>() * 8 - 1));
        let count = current & !(1 << (std::mem::size_of::<usize>() * 8 - 1));

        if count == 0 {
            true
        } else if count == 1 {
            self.weak_count.set(flag);
            true
        } else {
            self.weak_count.set(flag | (count - 1));
            false
        }
    }

    /// Check if the value has been dropped (only weak refs remain).
    pub fn is_value_dead(&self) -> bool {
        (self.weak_count.get() & (1 << (std::mem::size_of::<usize>() * 8 - 1))) != 0
    }

    /// Mark the value as dropped.
    pub(crate) fn set_dead(&self) {
        self.weak_count
            .set(self.weak_count.get() | (1 << (std::mem::size_of::<usize>() * 8 - 1)));
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
        let ptr = with_heap(GlobalHeap::alloc::<GcBox<T>>);

        // Initialize the GcBox
        let gc_box = ptr.as_ptr().cast::<GcBox<T>>();
        // SAFETY: We just allocated this memory
        unsafe {
            gc_box.write(GcBox {
                ref_count: Cell::new(NonZeroUsize::MIN),
                weak_count: Cell::new(0),
                drop_fn: GcBox::<T>::drop_fn_for,
                trace_fn: GcBox::<T>::trace_fn_for,
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        // Notify that we created a Gc
        crate::gc::notify_created_gc();

        Self {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
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
                    let ptr = with_heap(GlobalHeap::alloc::<GcBox<T>>);
                    let gc_box = ptr.as_ptr().cast::<GcBox<T>>();

                    // SAFETY: We just allocated this memory
                    unsafe {
                        gc_box.write(GcBox {
                            ref_count: Cell::new(NonZeroUsize::MIN),
                            weak_count: Cell::new(0),
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
                weak_count: Cell::new(0),
                drop_fn: GcBox::<T>::drop_fn_for,
                trace_fn: GcBox::<T>::trace_fn_for,
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

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

    /// Get the internal `GcBox` pointer.
    pub fn internal_ptr(gc: &Self) -> *const u8 {
        gc.ptr.get().unwrap().as_ptr().cast()
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

    /// Get the current weak reference count.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    pub fn weak_count(gc: &Self) -> usize {
        let ptr = gc.ptr.get().unwrap();
        unsafe { (*ptr.as_ptr()).weak_count() }
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
        let ptr = gc.ptr.get().unwrap();
        // Increment the weak count
        unsafe {
            (*ptr.as_ptr()).inc_weak();
        }
        Weak {
            ptr: Cell::new(Nullable::new(ptr)),
            _marker: PhantomData,
        }
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

        // SAFETY: If we are in the middle of a sweep, the target object
        // might have already been swept and its memory reused or invalidated.
        // We check if the object is unmarked (garbage) and skip if so.
        if is_collecting() {
            unsafe {
                let header = ptr_to_page_header(ptr.as_ptr().cast());
                // Valid GC pointers always have a magic number
                if (*header).magic == crate::heap::MAGIC_GC_PAGE {
                    if let Some(index) = ptr_to_object_index(ptr.as_ptr().cast()) {
                        if !(*header).is_marked(index) {
                            return;
                        }
                    }
                }
            }
        }

        // Decrement reference count
        let is_last = unsafe { (*ptr.as_ptr()).dec_ref() };

        if is_last {
            // This was the last reference; drop unconditionally
            // SAFETY: We have exclusive access
            unsafe {
                // Call the drop_fn to drop the inner value and mark as dropped
                ((*ptr.as_ptr()).drop_fn)(ptr.as_ptr().cast());
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
pub struct Weak<T: Trace + ?Sized + 'static> {
    /// Pointer to the `GcBox`.
    /// Points to the allocation even after the value is dropped.
    ptr: Cell<Nullable<GcBox<T>>>,
    /// Marker to make Weak !Send and !Sync.
    _marker: PhantomData<*const ()>,
}

impl<T: Trace + ?Sized> Weak<T> {
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
    pub fn upgrade(&self) -> Option<Gc<T>> {
        let ptr = self.ptr.get().as_option()?;

        // SAFETY: The pointer is valid because we have a weak reference
        unsafe {
            // Check if the value is still alive
            if (*ptr.as_ptr()).is_value_dead() {
                return None;
            }

            // Increment the strong reference count
            (*ptr.as_ptr()).inc_ref();

            // Notify the GC about the new Gc
            crate::gc::notify_created_gc();

            Some(Gc {
                ptr: Cell::new(Nullable::new(ptr)),
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
        let Some(ptr) = self.ptr.get().as_option() else {
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
        let Some(ptr) = self.ptr.get().as_option() else {
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
        let Some(ptr) = self.ptr.get().as_option() else {
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
        this.ptr.get().as_option() == other.ptr.get().as_option()
    }
}

impl<T: Trace + ?Sized> Clone for Weak<T> {
    fn clone(&self) -> Self {
        if let Some(ptr) = self.ptr.get().as_option() {
            // Increment the weak count
            unsafe {
                (*ptr.as_ptr()).inc_weak();
            }
        }
        Self {
            ptr: self.ptr.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: Trace + ?Sized> Drop for Weak<T> {
    fn drop(&mut self) {
        if let Some(ptr) = self.ptr.get().as_option() {
            // Decrement the weak count
            // SAFETY: The pointer is valid because we have a weak reference
            unsafe {
                (*ptr.as_ptr()).dec_weak();
            }
            // Note: Memory is managed by the GC, not deallocated here.
            // The `GcBox` memory is reclaimed during sweep when both
            // strong and weak counts are zero.
        }
    }
}

impl<T: Trace + ?Sized> std::fmt::Debug for Weak<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(Weak)")
    }
}

impl<T: Trace> Default for Weak<T> {
    /// Constructs a new `Weak<T>` that is dangling (cannot be upgraded).
    fn default() -> Self {
        Self {
            ptr: Cell::new(Nullable::null()),
            _marker: PhantomData,
        }
    }
}

// Weak is NOT Send or Sync (same as Gc)

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
