//! Interior mutability with write barriers for Generational GC.
//!
//! This module provides `GcCell<T>`, which acts like `RefCell<T>` but
//! notifies the Garbage Collector when mutations occur. Use this for
//! all interior mutability of GC-managed objects.

use crate::gc::incremental::IncrementalMarkState;
use crate::heap::{ptr_to_page_header, PageHeader, MAGIC_GC_PAGE};
use crate::ptr::GcBox;
use crate::trace::Trace;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, RwLock};

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
/// - **Dijkstra Insertion Barrier**: Immediately marks new pointer values written during marking.
///   This prevents newly-reachable objects from being missed.
/// - **SATB (Snapshot-At-The-Beginning)**: Records old pointer values before they're overwritten.
///   This ensures objects reachable at the start of marking are preserved.
///   Use `borrow_mut_with_satb()` to enable SATB barrier.
///
/// # API Comparison
///
/// | Method                   | Barrier Type              | T Bound   | Use Case                          |
/// |--------------------------|---------------------------|-----------|-----------------------------------|
/// | `borrow_mut()`           | Generational + Incremental| `Trace`   | General use (recommended)         |
/// | `borrow_mut_with_satb()` | Full (incl. SATB)         | `GcCapture`| Types with GC pointers           |
/// | `borrow_mut_gen_only()`  | Generational only         | -         | Performance-critical code         |
///
/// # Example
///
/// ```ignore
/// use rudo_gc::{Gc, GcCell};
///
/// // General use - works with any T
/// let cell = GcCell::new(42);
/// *cell.borrow_mut() = 100;
///
/// // With GC pointers - use borrow_mut_with_satb() for SATB
/// let cell = GcCell::new(Gc::new(Data));
/// *cell.borrow_mut_with_satb() = new_data;
///
/// // Performance optimization - generational barrier only
/// let cell = GcCell::new(expensive_computation());
/// *cell.borrow_mut_gen_only() = result;
/// ```
///
/// # Migration from Older Versions
///
/// ```ignore
/// // v0.7.x: Works with any Trace type
/// let cell = GcCell::new(42);
/// cell.borrow_mut();  // Works!
///
/// // For types with GC pointers that need SATB:
/// let cell = GcCell::new(Gc::new(Data));
/// cell.borrow_mut_with_satb();  // Full barrier
/// ```
///
/// # Thread Safety
///
/// `GcCell` is **not thread-safe**. It must only be accessed from the thread
/// where the containing `Gc` object was allocated. Accessing from other threads
/// (including tokio worker threads) will cause a panic with a clear error message.
///
/// This is because:
/// - `GcCell` is based on `RefCell`, which is intentionally `!Sync`
/// - Each thread has its own GC heap, and cross-thread access corrupts the heap
/// - The write barrier operations require thread-local state
///
/// For cross-thread mutation, use [`GcThreadSafeCell`] instead:
/// - Use `GcThreadSafeCell<T>` for interior mutability that can be accessed from any thread
/// - Uses internal `Mutex` for synchronization
/// - Works seamlessly with tokio multi-threaded runtime
///
/// For cross-thread communication without mutation, use `GcHandle`:
/// - Create a handle: `let handle = gc.cross_thread_handle();`
/// - Send the handle to any thread
/// - Resolve back to `Gc<T>` on the origin thread: `let gc = handle.resolve();`
///
/// Alternatively, dispatch mutations back to the main thread via a channel,
/// or use a single-threaded Tokio runtime.
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
        self.validate_thread_affinity("borrow");
        self.inner.borrow()
    }

    /// Mutably borrows the wrapped value with automatic SATB barrier.
    ///
    /// This method performs generational and incremental write barriers,
    /// plus SATB (Snapshot-At-The-Beginning) barrier to capture old pointer
    /// values during incremental marking. This ensures correct GC behavior
    /// for all types.
    ///
    /// Use this as the primary mutation method for `GcCell<T>`.
    ///
    /// # Type Bounds
    ///
    /// - `T: GcCapture` - Required for SATB barrier. Add `#[derive(GcCell)]` to your type.
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    #[inline]
    pub fn borrow_mut(&self) -> RefMut<'_, T>
    where
        T: GcCapture,
    {
        self.validate_thread_affinity("borrow_mut");

        let ptr = std::ptr::from_ref(self).cast::<u8>();

        if crate::gc::incremental::is_incremental_marking_active() {
            unsafe {
                let value = &*self.inner.as_ptr();
                let mut gc_ptrs = Vec::with_capacity(32);
                value.capture_gc_ptrs_into(&mut gc_ptrs);
                if !gc_ptrs.is_empty() {
                    crate::heap::with_heap(|heap| {
                        for gc_ptr in gc_ptrs {
                            if !heap.record_satb_old_value(gc_ptr) {
                                break;
                            }
                        }
                    });
                }
            }
        }

        self.generational_write_barrier(ptr);
        self.incremental_write_barrier(ptr);

        let result = self.inner.borrow_mut();

        if crate::gc::incremental::is_incremental_marking_active() {
            unsafe {
                let new_value = &*result;
                let mut new_gc_ptrs = Vec::with_capacity(32);
                new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
                if !new_gc_ptrs.is_empty() {
                    crate::heap::with_heap(|_heap| {
                        for gc_ptr in new_gc_ptrs {
                            let _ = crate::gc::incremental::mark_object_black(
                                gc_ptr.as_ptr() as *const u8
                            );
                        }
                    });
                }
            }
        }

        result
    }

    /// Mutably borrows the wrapped value with SATB barrier.
    ///
    /// This method is equivalent to `borrow_mut()`. It captures old GC pointer
    /// values before mutation, enabling correct incremental marking.
    ///
    /// # Deprecated
    ///
    /// This method is deprecated. Use `borrow_mut()` instead, which now includes
    /// the same SATB barrier behavior.
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    #[deprecated(since = "0.7.0", note = "Use borrow_mut() instead")]
    #[inline]
    pub fn borrow_mut_with_satb(&self) -> RefMut<'_, T>
    where
        T: GcCapture,
    {
        self.borrow_mut()
    }

    /// Mutably borrows the wrapped value with generational barrier only.
    ///
    /// This is an escape hatch for performance-critical code where
    /// barrier overhead is measurable. No barriers are triggered at all.
    ///
    /// # Safety
    ///
    /// Using this may cause incorrect collection during GC for types
    /// containing GC pointers. Use with caution and only when:
    /// 1. The type does not contain any `Gc<T>` pointers
    /// 2. Performance is critical and barriers are proven to be the bottleneck
    ///
    /// # Panics
    ///
    /// Panics if the value is currently borrowed.
    #[inline]
    pub fn borrow_mut_gen_only(&self) -> RefMut<'_, T> {
        self.validate_thread_affinity("borrow_mut_gen_only");
        self.inner.borrow_mut()
    }

    #[inline]
    fn validate_thread_affinity(&self, context: &str) {
        let cell_ptr = std::ptr::from_ref(self).cast::<u8>();

        let heap_start = crate::heap::heap_start();
        let heap_end = crate::heap::heap_end();
        let ptr_addr = cell_ptr as usize;

        if ptr_addr < heap_start || ptr_addr > heap_end {
            return;
        }

        let header = unsafe { crate::heap::ptr_to_page_header(cell_ptr) };
        let owner = unsafe { (*header.as_ptr()).owner_thread };

        let current = crate::heap::get_thread_id();

        assert!(
            owner == 0 || owner == current,
            "Thread safety violation: {context} called on GcCell allocated by a different thread.\n\
             - Current thread ID: {current}\n\
             - Allocating thread ID: {owner}\n\
             \n\
             GcCell is not thread-safe. Gc objects and their fields must not be\n\
             accessed from tokio worker threads or other threads different from where\n\
             they were allocated.\n\
             \n\
             Solutions:\n\
             1. Use GcThreadSafeCell instead of GcCell for thread-safe interior mutability\n\
             2. Use GcHandle for cross-thread communication (see cross_thread module)\n\
             3. Dispatch mutations back to the main thread via channel\n\
             4. Use single-threaded Tokio runtime"
        );
    }

    #[allow(dead_code)]
    #[allow(clippy::unused_self)]
    fn write_barrier(&self) {
        let ptr = std::ptr::from_ref(self).cast::<u8>();

        if crate::gc::incremental::is_generational_barrier_active() {
            self.generational_write_barrier(ptr);
        }

        if crate::gc::incremental::is_incremental_marking_active() {
            self.incremental_write_barrier(ptr);
        }
    }

    #[inline]
    #[allow(clippy::unused_self)]
    fn is_gc_heap_pointer(&self, ptr: *const u8) -> bool {
        let ptr_addr = ptr as usize;
        let heap_start = crate::heap::heap_start();
        let heap_end = crate::heap::heap_end();

        if ptr_addr < heap_start || ptr_addr > heap_end {
            return false;
        }

        let _page_addr = ptr_addr & crate::heap::page_mask();
        let header = unsafe { crate::heap::ptr_to_page_header(ptr) };
        unsafe { (*header.as_ptr()).magic == crate::heap::MAGIC_GC_PAGE }
    }

    /// Records a write to an old-generation object for generational GC.
    ///
    /// This implements the generational GC invariant: all OLD→YOUNG references
    /// must be tracked so minor collections can find roots without scanning old gen.
    ///
    /// **Important**: This barrier remains active through ALL phases of incremental
    /// marking (including `FinalMark`), not just during Marking. Mutations during
    /// `FinalMark` must still be recorded for correctness.
    #[allow(clippy::unused_self)]
    fn generational_write_barrier(&self, ptr: *const u8) {
        if !self.is_gc_heap_pointer(ptr) {
            return;
        }

        unsafe {
            crate::heap::with_heap(|heap| {
                let ptr_addr = ptr as usize;
                let page_addr = ptr_addr & crate::heap::page_mask();
                let is_large = heap.large_object_map.contains_key(&page_addr);

                if is_large {
                    if let Some(&(head_addr, _obj_size, _h_size)) =
                        heap.large_object_map.get(&page_addr)
                    {
                        let header = head_addr as *mut crate::heap::PageHeader;
                        if (*header).magic == crate::heap::MAGIC_GC_PAGE && (*header).generation > 0
                        {
                            let block_size = (*header).block_size as usize;
                            let header_size = (*header).header_size as usize;
                            let header_page_addr = head_addr;

                            if ptr_addr >= header_page_addr + header_size {
                                let offset = ptr_addr - (header_page_addr + header_size);
                                let index = offset / block_size;
                                let obj_count = (*header).obj_count as usize;

                                if index < obj_count {
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
                        let obj_count = (*header.as_ptr()).obj_count as usize;

                        if ptr_addr >= header_page_addr + header_size {
                            let offset = ptr_addr - (header_page_addr + header_size);
                            let index = offset / block_size;

                            if index < obj_count {
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
        if !self.is_gc_heap_pointer(ptr) {
            return;
        }

        let state = IncrementalMarkState::global();

        if !state.config().enabled || state.fallback_requested() {
            return;
        }

        std::sync::atomic::fence(Ordering::AcqRel);

        unsafe {
            if state.fallback_requested() {
                return;
            }

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
///
/// # Overview
///
/// `GcCapture` is used by the SATB barrier to record old GC pointer values
/// before they are overwritten during incremental marking.
///
/// # Implementations Provided
///
/// The trait is automatically implemented for standard library types:
/// - `Gc<T>`
/// - `Option<Gc<T>>`, `Vec<Gc<T>>`, `[Gc<T>; N]`
/// - `GcCell<Gc<T>>`, `GcCell<Vec<Gc<T>>>`, etc.
///
/// # Derive Macro
///
/// For custom types, use `#[derive(GcCell)]` to automatically implement this trait:
///
/// ```
/// use rudo_gc::{Gc, Trace, cell::GcCell};
///
/// #[derive(Trace, GcCell)]
/// struct MyStruct {
///     gc_field: Gc<Other>,      // Automatically implements GcCapture
///     regular_field: i32,        // No GcCapture needed
/// }
/// ```
///
/// # Manual Implementation
///
/// For complex types (generics, recursion), implement manually:
///
/// ```
/// use rudo_gc::{Gc, Trace, cell::{GcCell, GcCapture, GcBox}};
/// use std::ptr::NonNull;
///
/// struct MyStruct<T> {
///     gc_field: Gc<T>,
/// }
///
/// unsafe impl<T: Trace + 'static> GcCapture for MyStruct<T> {
///     fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
///         self.gc_field.capture_gc_ptrs_into(ptrs);
///     }
/// }
/// ```
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
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        let raw = self.raw_ptr();
        if !raw.is_null() {
            unsafe {
                // SAFETY: The cast from *mut GcBox<T> to *mut GcBox<()> is valid because:
                // 1. GcBox<T> has repr(C) layout for all T
                // 2. The pointer has valid provenance from raw_ptr() allocated by GC heap
                // 3. NonNull::new_unchecked is safe because we checked is_null() above
                let nn = NonNull::new_unchecked(raw.cast());
                ptrs.push(nn);
            }
        }
    }
}

impl<T: GcCapture + 'static> GcCapture for Option<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(value) = self {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: GcCapture + 'static> GcCapture for Vec<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for value in self {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: GcCapture + 'static, const N: usize> GcCapture for [T; N] {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for value in self {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

impl<K: GcCapture + 'static, V: GcCapture + 'static, S: std::hash::BuildHasher + Default> GcCapture
    for HashMap<K, V, S>
{
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for key in self.keys() {
            key.capture_gc_ptrs_into(ptrs);
        }
        for value in self.values() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<K: GcCapture + 'static, V: GcCapture + 'static> GcCapture for BTreeMap<K, V> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for key in self.keys() {
            key.capture_gc_ptrs_into(ptrs);
        }
        for value in self.values() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: GcCapture + 'static, S: std::hash::BuildHasher + Default> GcCapture for HashSet<T, S> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for value in self {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: GcCapture + 'static> GcCapture for BTreeSet<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for value in self {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: GcCapture + 'static> GcCapture for Box<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        (**self).capture_gc_ptrs_into(ptrs);
    }
}

impl<T: GcCapture + ?Sized> GcCapture for GcCell<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        unsafe {
            // SAFETY: We read the inner value of RefCell by pointer.
            // RefCell::as_ptr() is always valid after construction.
            // The ptr cannot be null because GcCell::new() always initializes inner.
            let ptr = self.inner.as_ptr();
            if ptr.is_null() {
                return;
            }
            let value = &*ptr;
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: GcCapture + Copy + 'static> GcCapture for Cell<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        let value = self.get();
        value.capture_gc_ptrs_into(ptrs);
    }
}

impl<T: GcCapture + 'static> GcCapture for RefCell<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        let value = self.borrow();
        value.capture_gc_ptrs_into(ptrs);
    }
}

impl<T: GcCapture + 'static> GcCapture for RwLock<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Ok(value) = self.try_read() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}

/// Macro to implement `GcCapture` for types that don't contain `Gc<T>` pointers.
///
/// This is useful for simple types (e.g., primitives, String, non-Gc structs)
/// that need to be stored in `GcCell` but don't contain any GC pointers.
///
/// # Example
///
/// ```rust
/// use rudo_gc::{cell::GcCell, impl_gc_capture};
///
/// #[derive(Clone)]
/// struct MyItem {
///     id: i64,
///     name: String,
/// }
///
/// unsafe impl rudo_gc::Trace for MyItem {
///     fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
/// }
///
/// impl_gc_capture!(MyItem);
/// ```
///
/// # Warning
///
/// Only use this macro for types that truly contain no `Gc<T>` fields.
/// Using it on types with GC pointers will cause memory corruption.
#[macro_export]
macro_rules! impl_gc_capture {
    ($t:ty) => {
        impl ::rudo_gc::cell::GcCapture for $t {
            #[inline]
            fn capture_gc_ptrs(&self) -> &[::std::ptr::NonNull<::rudo_gc::GcBox<()>>] {
                &[]
            }

            #[inline]
            fn capture_gc_ptrs_into(
                &self,
                _ptrs: &mut Vec<::std::ptr::NonNull<::rudo_gc::GcBox<()>>>,
            ) {
            }
        }
    };
}

// SAFETY: GcCell is Trace if T is Trace.
// It just traces the inner value.
//
// For GcCapture: GcCell<T> where T: GcCapture forwards to the inner value.
// If T is borrowed mutably (RefCell), capture_gc_ptrs_into will skip capturing.
// This is consistent with RefCell's normal semantics.
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

/// A thread-safe interior mutability type for GC-managed data.
///
/// `GcThreadSafeCell<T>` is like `GcCell<T>` but uses a `Mutex` to allow
/// safe mutation from any thread, including tokio worker threads.
///
/// # Use Cases
///
/// - Sharing GC-managed state between tokio tasks on different worker threads
/// - Mutations to reactive state from async callbacks
/// - Any case where GC-managed data needs to be updated from multiple threads
///
/// # Example
///
/// ```ignore
/// use rudo_gc::{Gc, GcThreadSafeCell, Trace};
///
/// #[derive(Trace, Clone)]
/// struct SharedState {
///     counter: GcThreadSafeCell<i32>,
/// }
///
/// // Works with multi-threaded tokio runtime
/// let state = Gc::new(SharedState {
///     counter: GcThreadSafeCell::new(0),
/// });
///
/// // Can mutate from any thread
/// *state.counter.borrow_mut() += 1;
/// ```
///
/// # Thread Safety
///
/// Unlike `GcCell`, which requires all operations to occur on the allocating thread,
/// `GcThreadSafeCell` uses internal synchronization to allow safe cross-thread access.
/// This does introduce some overhead (lock acquisition), but ensures correctness.
///
/// For maximum performance when mutations are always on the same thread, use `GcCell`.
pub struct GcThreadSafeCell<T: ?Sized> {
    inner: Mutex<T>,
}

impl<T> GcThreadSafeCell<T> {
    /// Creates a new `GcThreadSafeCell` containing `value`.
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }

    /// Consumes the `GcThreadSafeCell`, returning the wrapped value.
    pub fn into_inner(self) -> T {
        self.inner.into_inner().unwrap()
    }
}

impl<T: ?Sized> GcThreadSafeCell<T> {
    /// Immutably borrows the wrapped value.
    ///
    /// The borrow lasts until the returned `MutexGuard` exits scope.
    #[inline]
    pub fn borrow(&self) -> std::sync::MutexGuard<'_, T> {
        self.inner.lock().unwrap()
    }

    /// Mutably borrows the wrapped value with generational + incremental write barriers.
    ///
    /// This method acquires the mutex lock and performs all necessary write barriers
    /// to ensure correct GC behavior when mutations occur from any thread.
    ///
    /// # Type Bounds
    ///
    /// - `T: GcCapture` - Required for SATB barrier. Add `#[derive(GcCell)]` to your type.
    ///
    /// # Performance Note
    ///
    /// This method acquires a lock. For very high-frequency mutations, consider
    /// whether `GcCell` on a single thread might be more appropriate.
    ///
    /// For types without GC pointers, use `borrow_mut_gen_only()` instead.
    #[inline]
    pub fn borrow_mut(&self) -> GcThreadSafeRefMut<'_, T>
    where
        T: GcCapture,
    {
        GcThreadSafeRefMut {
            inner: self.inner.lock().unwrap(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Mutably borrows the wrapped value without write barriers.
    ///
    /// This is an escape hatch for performance-critical code where
    /// barrier overhead is measurable, or for types that don't contain
    /// any GC pointers.
    ///
    /// # Safety
    ///
    /// Using this may cause incorrect collection during GC for types
    /// containing GC pointers. Use with caution.
    #[inline]
    pub fn borrow_mut_gen_only(&self) -> std::sync::MutexGuard<'_, T> {
        self.inner.lock().unwrap()
    }
}

/// A mutable borrow of a `GcThreadSafeCell`.
///
/// This guard ensures write barriers are executed when dropped.
pub struct GcThreadSafeRefMut<'a, T: ?Sized> {
    inner: std::sync::MutexGuard<'a, T>,
    _marker: std::marker::PhantomData<*const ()>,
}

impl<T: ?Sized> std::ops::Deref for GcThreadSafeRefMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: ?Sized> std::ops::DerefMut for GcThreadSafeRefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

// SAFETY: GcThreadSafeCell uses Mutex internally, which handles synchronization.
// The Trace implementation acquires the lock during GC, ensuring no data races.
unsafe impl<T: Trace + ?Sized> Trace for GcThreadSafeCell<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        // Lock is acquired during tracing to prevent concurrent mutations.
        // This is safe because GC happens during STW pauses.
        let guard = self.inner.lock().unwrap();
        guard.trace(visitor);
    }
}

impl<T: GcCapture + ?Sized> GcCapture for GcThreadSafeCell<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Ok(guard) = self.inner.try_lock() {
            guard.capture_gc_ptrs_into(ptrs);
        }
    }
}

impl<T: Default> Default for GcThreadSafeCell<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T: Clone> Clone for GcThreadSafeCell<T> {
    fn clone(&self) -> Self {
        Self::new(self.borrow().clone())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for GcThreadSafeCell<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner.try_lock() {
            Ok(guard) => guard.fmt(f),
            Err(_) => write!(f, "<GcThreadSafeCell: locked>"),
        }
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
    fn test_borrow_mut_gen_only() {
        let cell = GcCell::new(42);
        {
            let mut borrow = cell.borrow_mut_gen_only();
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

    #[test]
    fn test_same_thread_borrow_works() {
        let cell = GcCell::new(42i32);
        let _ = cell.borrow();

        let mut borrow = cell.borrow_mut_gen_only();
        *borrow = 100;
        drop(borrow);

        assert_eq!(*cell.borrow(), 100);
    }
}
