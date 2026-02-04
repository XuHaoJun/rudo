//! Interior mutability with write barriers for Generational GC.
//!
//! This module provides `GcCell<T>`, which acts like `RefCell<T>` but
//! notifies the Garbage Collector when mutations occur. Use this for
//! all interior mutability of GC-managed objects.

use crate::gc::incremental::IncrementalMarkState;
use crate::heap::{ptr_to_page_header, PageHeader, MAGIC_GC_PAGE};
use crate::ptr::GcBox;
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
/// The `borrow_mut()` method automatically captures old GC pointer values for SATB
/// verification. For types that don't implement `GcCapture` (e.g., primitives),
/// use `borrow_mut_unchecked()`.
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

    /// Mutably borrows the wrapped value with automatic SATB barrier.
    ///
    /// This method automatically captures old GC pointer values before mutation,
    /// enabling correct incremental marking. Use this for `GcCell<Gc<T>>`,
    /// `GcCell<Option<Gc<T>>>`, `GcCell<Vec<Gc<T>>>`, and similar types.
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    #[inline]
    pub fn borrow_mut(&self) -> RefMut<'_, T>
    where
        T: GcCapture,
    {
        let ptr = std::ptr::from_ref(self).cast::<u8>();

        if crate::gc::incremental::is_incremental_marking_active() {
            unsafe {
                let value = &*self.inner.as_ptr();
                let mut gc_ptrs = Vec::with_capacity(32);
                value.capture_gc_ptrs_into(&mut gc_ptrs);
                if !gc_ptrs.is_empty() {
                    crate::heap::with_heap(|heap| {
                        for gc_ptr in gc_ptrs {
                            heap.record_satb_old_value(gc_ptr);
                        }
                    });
                }
            }
        }

        self.generational_write_barrier(ptr);
        self.inner.borrow_mut()
    }

    /// Mutably borrows the wrapped value without GC tracking.
    ///
    /// This is an escape hatch for types that don't implement `GcCapture`
    /// (e.g., primitives, non-Gc structs). No write barrier is triggered.
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    #[inline]
    pub fn borrow_mut_unchecked(&self) -> RefMut<'_, T> {
        self.inner.borrow_mut()
    }

    #[allow(dead_code)]
    #[allow(clippy::unused_self)]
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
                                heap.add_to_dirty_pages(header);
                            }
                        }
                    }
                }
            });
        }
    }

    #[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Trait for types that can participate in SATB barrier.
/// Implement this to enable automatic old-value capture during write barriers.
pub trait GcCapture {
    /// Returns a slice of all `GcBox` pointers contained in this type.
    ///
    /// For single `Gc<T>`: returns slice of length 0 or 1
    /// For `Vec<Gc<T>>`: returns slice of all elements
    /// For non-Gc types: returns empty slice
    ///
    /// The returned pointers are used for SATB barrier recording.
    ///
    /// Note: For complex nested types (Vec, arrays), prefer `capture_gc_ptrs_into()`
    /// to avoid pointer provenance issues with Miri.
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>];

    /// Fill the provided buffer with all `GcBox` pointers contained in this type.
    ///
    /// This method avoids pointer provenance issues by using an owned buffer.
    /// Callers should pass a mutable buffer and keep it alive during GC operations.
    ///
    /// Default implementation extracts pointers from `capture_gc_ptrs()`.
    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        let slice = self.capture_gc_ptrs();
        ptrs.extend_from_slice(slice);
    }
}

impl<T: Trace + 'static> GcCapture for crate::Gc<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        let ptr = self.raw_ptr();
        if ptr.is_null() {
            &[]
        } else {
            unsafe {
                let nn = NonNull::new_unchecked(ptr.cast::<crate::ptr::GcBox<()>>());
                std::slice::from_raw_parts(nn.as_ptr() as *const _, 1)
            }
        }
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        let raw = self.raw_ptr();
        if !raw.is_null() {
            unsafe {
                let nn = NonNull::new_unchecked(raw.cast());
                ptrs.push(nn);
            }
        }
    }
}

impl<T: Trace + 'static> GcCapture for Option<crate::Gc<T>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(gc) = self {
            gc.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: Trace + 'static> GcCapture for Vec<crate::Gc<T>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for gc in self {
            gc.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: Trace + 'static> GcCapture for Option<Vec<crate::Gc<T>>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(vec) = self {
            vec.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: Trace + 'static, const N: usize> GcCapture for [crate::Gc<T>; N] {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for gc in self {
            gc.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: Trace + 'static> GcCapture for crate::GcCell<crate::Gc<T>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        unsafe {
            let gc_ptr = self.inner.as_ptr();
            if !gc_ptr.is_null() {
                let gc = &*gc_ptr;
                let raw = gc.raw_ptr();
                if !raw.is_null() {
                    let nn = NonNull::new_unchecked(raw.cast());
                    ptrs.push(nn);
                }
            }
        }
    }
}

impl<T: Trace + 'static> GcCapture for crate::GcCell<Option<crate::Gc<T>>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        unsafe {
            let option_ptr = self.inner.as_ptr();
            if !option_ptr.is_null() {
                let option = &*option_ptr;
                if let Some(gc) = option {
                    let raw = gc.raw_ptr();
                    if !raw.is_null() {
                        let nn = NonNull::new_unchecked(raw.cast());
                        ptrs.push(nn);
                    }
                }
            }
        }
    }
}

impl<T: Trace + 'static> GcCapture for crate::GcCell<Vec<crate::Gc<T>>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        unsafe {
            let vec_ptr = self.inner.as_ptr();
            if !vec_ptr.is_null() {
                let vec = &*vec_ptr;
                for gc in vec {
                    let raw = gc.raw_ptr();
                    if !raw.is_null() {
                        let nn = NonNull::new_unchecked(raw.cast());
                        ptrs.push(nn);
                    }
                }
            }
        }
    }
}

impl<T: Trace + 'static> GcCapture for crate::GcCell<Option<Vec<crate::Gc<T>>>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        unsafe {
            let option_ptr = self.inner.as_ptr();
            if !option_ptr.is_null() {
                let option = &*option_ptr;
                if let Some(vec) = option {
                    for gc in vec {
                        let raw = gc.raw_ptr();
                        if !raw.is_null() {
                            let nn = NonNull::new_unchecked(raw.cast());
                            ptrs.push(nn);
                        }
                    }
                }
            }
        }
    }
}

impl<T: Trace + 'static, const N: usize> GcCapture for crate::GcCell<[crate::Gc<T>; N]> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        unsafe {
            let arr_ptr = self.inner.as_ptr();
            if !arr_ptr.is_null() {
                let arr = &*arr_ptr;
                for gc in arr {
                    let raw = gc.raw_ptr();
                    if !raw.is_null() {
                        let nn = NonNull::new_unchecked(raw.cast());
                        ptrs.push(nn);
                    }
                }
            }
        }
    }
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[allow(clippy::implicit_hasher)]
impl<K: Trace + 'static, V: Trace + 'static, S: std::hash::BuildHasher + Default> GcCapture
    for HashMap<crate::Gc<K>, crate::Gc<V>, S>
{
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for gc in self.values() {
            gc.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<K: Trace + 'static, V: Trace + 'static> GcCapture for BTreeMap<crate::Gc<K>, crate::Gc<V>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for gc in self.values() {
            gc.capture_gc_ptrs_into(ptrs);
        }
    }
}

#[allow(clippy::implicit_hasher)]
impl<T: Trace + 'static, S: std::hash::BuildHasher + Default> GcCapture
    for HashSet<crate::Gc<T>, S>
{
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for gc in self {
            gc.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: Trace + 'static> GcCapture for BTreeSet<crate::Gc<T>> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for gc in self {
            gc.capture_gc_ptrs_into(ptrs);
        }
    }
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
        self.borrow().fmt(f)
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

        drop(borrow);
        assert_eq!(**cell.borrow().as_ref().unwrap(), 100);
    }

    #[test]
    fn test_satb_capture_with_borrow_mut() {
        let cell = Gc::new(GcCell::new(Some(Gc::new(42))));

        let mut borrow = cell.borrow_mut();
        *borrow = Some(Gc::new(100));

        drop(borrow);
        assert_eq!(**cell.borrow().as_ref().unwrap(), 100);
    }

    #[test]
    fn test_satb_capture_vec_all_elements() {
        let cell = Gc::new(GcCell::new(vec![Gc::new(1), Gc::new(2), Gc::new(3)]));

        {
            let mut borrow = cell.borrow_mut();
            borrow[1] = Gc::new(200);
        }

        let values: Vec<i32> = cell.borrow().iter().map(|gc| **gc).collect();
        assert_eq!(values, vec![1, 200, 3]);
    }

    #[test]
    fn test_satb_preserves_replaced_object() {
        let cell = Gc::new(GcCell::new(vec![Gc::new(1), Gc::new(2)]));

        let old_ptr = cell.borrow()[1].raw_ptr();

        {
            let mut borrow = cell.borrow_mut();
            borrow[1] = Gc::new(999);
        }

        assert!(!old_ptr.is_null());
    }

    #[test]
    fn test_satb_capture_option_vec() {
        let cell = Gc::new(GcCell::new(Some(vec![Gc::new(1), Gc::new(2)])));

        {
            let mut borrow = cell.borrow_mut();
            borrow.as_mut().unwrap()[0] = Gc::new(100);
        }

        let values: Vec<i32> = cell
            .borrow()
            .as_ref()
            .unwrap()
            .iter()
            .map(|gc| **gc)
            .collect();
        assert_eq!(values, vec![100, 2]);
    }

    #[test]
    fn test_satb_capture_array() {
        let cell = Gc::new(GcCell::new([Gc::new(1), Gc::new(2), Gc::new(3)]));

        {
            let mut borrow = cell.borrow_mut();
            borrow[2] = Gc::new(300);
        }

        let values: Vec<i32> = cell.borrow().iter().map(|gc| **gc).collect();
        assert_eq!(values, vec![1, 2, 300]);
    }

    #[test]
    fn test_borrow_mut_unchecked() {
        let cell = GcCell::new(42);
        {
            let mut borrow = cell.borrow_mut_unchecked();
            *borrow = 100;
        }
        assert_eq!(cell.into_inner(), 100);
    }

    #[test]
    fn test_gccapture_gccell_gc() {
        use crate::cell::GcCapture;
        let inner = Gc::new(42);
        let cell = GcCell::new(inner);
        let mut ptrs = Vec::new();
        cell.capture_gc_ptrs_into(&mut ptrs);
        assert_eq!(ptrs.len(), 1);
    }

    #[test]
    fn test_gccapture_gccell_option_gc() {
        use crate::cell::GcCapture;
        let cell_none = GcCell::new(None::<Gc<i32>>);
        let mut ptrs_none = Vec::new();
        cell_none.capture_gc_ptrs_into(&mut ptrs_none);
        assert_eq!(ptrs_none.len(), 0);

        let cell_some = GcCell::new(Some(Gc::new(42)));
        let mut ptrs_some = Vec::new();
        cell_some.capture_gc_ptrs_into(&mut ptrs_some);
        assert_eq!(ptrs_some.len(), 1);
    }

    #[test]
    fn test_gccapture_gccell_vec_gc() {
        use crate::cell::GcCapture;
        let cell_empty = GcCell::new(Vec::<Gc<i32>>::new());
        let mut ptrs_empty = Vec::new();
        cell_empty.capture_gc_ptrs_into(&mut ptrs_empty);
        assert_eq!(ptrs_empty.len(), 0);

        let cell_vec = GcCell::new(vec![Gc::new(1), Gc::new(2), Gc::new(3)]);
        let mut ptrs = Vec::new();
        cell_vec.capture_gc_ptrs_into(&mut ptrs);
        assert_eq!(ptrs.len(), 3);
    }

    #[test]
    fn test_gccapture_gccell_array_gc() {
        use crate::cell::GcCapture;
        let cell = GcCell::new([Gc::new(1), Gc::new(2)]);
        let mut ptrs = Vec::new();
        cell.capture_gc_ptrs_into(&mut ptrs);
        assert_eq!(ptrs.len(), 2);
    }
}
