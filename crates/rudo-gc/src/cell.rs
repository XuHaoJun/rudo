//! Interior mutability with write barriers for Generational GC.
//!
//! This module provides `GcCell<T>`, which acts like `RefCell<T>` but
//! notifies the Garbage Collector when mutations occur. Use this for
//! all interior mutability of GC-managed objects.

use crate::heap::{ptr_to_object_index, ptr_to_page_header};
use crate::trace::Trace;
use std::cell::{Ref, RefCell, RefMut};

/// A memory location with interior mutability that triggers a write barrier.
///
/// `GcCell<T>` is equivalent to `RefCell<T>` but is aware of the garbage collector.
/// It must be used for any mutable `Gc<T>` fields to ensure that the GC can
/// track references from old-generation objects to new-generation objects.
///
/// # Generational GC and Write Barriers
///
/// In a generational GC, we want to collect the young generation frequently without
/// scanning the entire old generation. However, if an old object is mutated to
/// point to a young object, the GC needs to know about it.
///
/// `GcCell` solves this by checking if it lives in an old page during mutation.
/// If it does, it sets a "dirty bit" for its object in the page header. The GC
/// then treats dirty objects as roots for the next minor collection.
pub struct GcCell<T: ?Sized> {
    inner: RefCell<T>,
}

impl<T> GcCell<T> {
    /// Creates a new `GcCell` containing `value`.
    pub const fn new(value: T) -> Self {
        Self {
            inner: RefCell::new(value),
        }
    }

    /// Consumes the `GcCell`, returning the wrapped value.
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: ?Sized> GcCell<T> {
    /// Immutably borrows the wrapped value.
    ///
    /// The borrow lasts until the returned `Ref` exits scope. Multiple immutable borrows
    /// can be taken out at the same time.
    ///
    /// # Panics
    ///
    /// Panics if the value is currently mutably borrowed.
    #[inline]
    pub fn borrow(&self) -> Ref<'_, T> {
        self.inner.borrow()
    }

    /// Mutably borrows the wrapped value.
    ///
    /// The borrow lasts until the returned `RefMut` exits scope. The value cannot be
    /// borrowed while this borrow is active.
    ///
    /// Triggers a write barrier to notify the GC of potential old-to-young pointers.
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    #[inline]
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.write_barrier();
        self.inner.borrow_mut()
    }

    /// Triggers the write barrier for this cell.
    ///
    /// This checks if the cell is in an old generation page and marks it as dirty if so.
    fn write_barrier(&self) {
        // We need to find if we are in a GC page and if that page is Old.
        // SAFETY: self points to valid memory.
        let ptr = std::ptr::from_ref(self).cast::<u8>();
        unsafe {
            let header = ptr_to_page_header(ptr);
            if !header.is_null() {
                // We are in a GC page.
                // Check generation.
                if (*header).generation > 0 {
                    // We are in an old page. We must record this write.
                    // Find our object index.
                    if let Some(index) = ptr_to_object_index(ptr) {
                        (*header).set_dirty(index);
                    }
                }
            }
        }
    }
}

// SAFETY: GcCell is Trace if T is Trace.
// It just traces the inner value.
unsafe impl<T: Trace + ?Sized> Trace for GcCell<T> {
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        self.inner.borrow().trace(visitor);
    }
}

// Implement standard traits
impl<T: Default> Default for GcCell<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T: Clone> Clone for GcCell<T> {
    fn clone(&self) -> Self {
        // Note: Clone creates a NEW object, which starts Young.
        // So no write barrier needed for the *new* object.
        Self::new(self.borrow().clone())
    }
}

impl<T: std::fmt::Debug + ?Sized> std::fmt::Debug for GcCell<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

impl<T: std::fmt::Display + ?Sized> std::fmt::Display for GcCell<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.borrow().fmt(f)
    }
}
