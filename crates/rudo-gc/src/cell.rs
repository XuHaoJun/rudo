//! Interior mutability with write barriers for Generational GC.
//!
//! This module provides `GcCell<T>`, which acts like `RefCell<T>` but
//! notifies the Garbage Collector when mutations occur. Use this for
//! all interior mutability of GC-managed objects.

use crate::gc::incremental::IncrementalMarkState;
use crate::heap::{ptr_to_page_header, PageHeader, MAGIC_GC_PAGE};
use crate::trace::Trace;
use std::cell::{Ref, RefCell, RefMut};
use std::ptr::NonNull;

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
///
/// # Incremental GC and SATB Barriers
///
/// During incremental marking, `GcCell` implements a hybrid SATB + Dijkstra barrier:
///
/// - **SATB (Snapshot-At-The-Beginning)**: Records old pointer values before they're overwritten.
///   This ensures objects reachable at the start of marking are preserved.
/// - **Dijkstra Insertion Barrier**: Immediately marks new pointer values written during marking.
///   This prevents newly-reachable objects from being missed.
///
/// The write barrier is only active during the `Marking` phase of incremental collection.
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
    /// During incremental marking, also applies SATB + Dijkstra barriers.
    fn write_barrier(&self) {
        let ptr = std::ptr::from_ref(self).cast::<u8>();

        if crate::gc::incremental::is_incremental_marking_active() {
            self.incremental_write_barrier(ptr);
        } else {
            self.generational_write_barrier(ptr);
        }
    }

    #[allow(clippy::unused_self)]
    fn generational_write_barrier(&self, ptr: *const u8) {
        unsafe {
            crate::heap::with_heap(|heap| {
                let page_addr = (ptr as usize) & crate::heap::page_mask();
                let is_large = heap.large_object_map.contains_key(&page_addr);

                if is_large {
                    if let Some(&(head_addr, _, _)) = heap.large_object_map.get(&page_addr) {
                        let header = head_addr as *mut crate::heap::PageHeader;
                        if (*header).magic == crate::heap::MAGIC_GC_PAGE && (*header).generation > 0
                        {
                            let block_size = (*header).block_size as usize;
                            let header_size = (*header).header_size as usize;
                            let header_page_addr = head_addr;
                            let ptr_addr = ptr as usize;

                            if ptr_addr >= header_page_addr + header_size {
                                let offset = ptr_addr - (header_page_addr + header_size);
                                let index = offset / block_size;

                                if index < (*header).obj_count as usize {
                                    (*header).set_dirty(index);
                                    // Add page to dirty list for minor GC optimization
                                    heap.add_to_dirty_pages(NonNull::new_unchecked(header));
                                }
                            }
                        }
                    }
                } else {
                    let header = crate::heap::ptr_to_page_header(ptr);
                    if (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE
                        && (*header.as_ptr()).generation > 0
                    {
                        let block_size = (*header.as_ptr()).block_size as usize;
                        let header_size = (*header.as_ptr()).header_size as usize;
                        let header_page_addr = header.as_ptr() as usize;
                        let ptr_addr = ptr as usize;

                        if ptr_addr >= header_page_addr + header_size {
                            let offset = ptr_addr - (header_page_addr + header_size);
                            let index = offset / block_size;

                            if index < (*header.as_ptr()).obj_count as usize {
                                (*header.as_ptr()).set_dirty(index);
                                // Add page to dirty list for minor GC optimization
                                heap.add_to_dirty_pages(header);
                            }
                        }
                    }
                }
            });
        }
    }

    #[allow(clippy::unused_self)]
    #[allow(clippy::needless_return)]
    fn incremental_write_barrier(&self, ptr: *const u8) {
        let state = IncrementalMarkState::global();

        if !state.config().enabled || state.fallback_requested() {
            return;
        }

        unsafe {
            let header = ptr_to_page_header(ptr);
            if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
                return;
            }

            if (*header.as_ptr()).generation > 0 {
                let _ = record_page_in_remembered_buffer(header);
            }
        }
    }
}

/// Record a page in the thread's remembered buffer.
///
/// This is used by the SATB barrier to record pages that may contain
/// overwritten old values. The remembered buffer is flushed to the
/// global dirty list when it overflows.
#[allow(unsafe_op_in_unsafe_fn)]
#[inline]
unsafe fn record_page_in_remembered_buffer(page: NonNull<PageHeader>) -> bool {
    crate::heap::with_heap(|heap| {
        let header = page.as_ptr();
        if (*header).generation > 0 {
            heap.record_in_remembered_buffer(page);
            true
        } else {
            false
        }
    })
}

// SAFETY: GcCell is Trace if T is Trace.
// It just traces the inner value.
unsafe impl<T: Trace + ?Sized> Trace for GcCell<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        // SAFETY:
        // 1. GC happens during Stop-The-World (STW), all mutator threads are paused
        // 2. There may be active RefMut on the stack, but there won't be concurrent writes
        //    during GC scanning
        // 3. We only read fields for marking, we don't modify RefCell's internal state
        // 4. RefCell::as_ptr() is safe and doesn't panic
        let ptr = self.inner.as_ptr();
        unsafe {
            (*ptr).trace(visitor);
        }
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

#[cfg(test)]
mod tests {
    use crate::{cell::GcCell, Gc};

    #[test]
    fn test_gc_during_borrow_mut() {
        let cell = Gc::new(GcCell::new(Some(Gc::new(42))));

        let mut borrow = cell.borrow_mut();
        *borrow = Some(Gc::new(100));

        crate::collect_full();

        drop(borrow);
        assert_eq!(**cell.borrow().as_ref().unwrap(), 100);
    }
}
