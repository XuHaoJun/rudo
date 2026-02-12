//! Trace trait and Visitor pattern for garbage collection.
//!
//! Types that implement `Trace` can be stored in `Gc<T>` and will be
//! automatically traversed during garbage collection.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, LinkedList, VecDeque};
use std::hash::BuildHasher;
use std::rc::Rc;
use std::sync::Arc;

use crate::ptr::GcBox;
use crate::Gc;

// ============================================================================
// Core Traits
// ============================================================================

/// A type that can be traced by the garbage collector.
///
/// # Safety
///
/// Implementations **MUST** correctly report all `Gc<T>` fields by calling
/// `visitor.visit()` on each one. Failure to do so will result in
/// use-after-free or memory leaks.
///
/// Prefer using `#[derive(Trace)]` instead of manual implementation.
///
/// # Examples
///
/// For primitive types that don't contain `Gc` pointers:
///
/// ```ignore
/// unsafe impl Trace for MyPrimitive {
///     fn trace(&self, _visitor: &mut impl Visitor) {
///         // No Gc fields, nothing to trace
///     }
/// }
/// ```
///
/// For types containing `Gc` fields:
///
/// ```ignore
/// unsafe impl Trace for MyStruct {
///     fn trace(&self, visitor: &mut impl Visitor) {
///         self.gc_field.trace(visitor);
///         self.another_gc.trace(visitor);
///     }
/// }
/// ```
pub unsafe trait Trace {
    /// Visit all `Gc` pointers contained within this value.
    ///
    /// The visitor will be called with each `Gc<T>` field. The implementation
    /// must visit ALL Gc fields, including those inside nested structs, enums,
    /// and collections.
    fn trace(&self, visitor: &mut impl Visitor);
}

/// A visitor that traverses the object graph during garbage collection.
///
/// Users generally do not need to implement this trait. It is used internally
/// by the garbage collector to mark reachable objects.
pub trait Visitor {
    /// Visit a garbage-collected pointer.
    ///
    /// Called by `Trace::trace()` for each `Gc` field in an object.
    fn visit<T: Trace>(&mut self, gc: &Gc<T>);

    /// Visit a memory region conservatively for potential `Gc` pointers.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for reading `len` bytes.
    unsafe fn visit_region(&mut self, ptr: *const u8, len: usize);
}

// ============================================================================
// Concrete Visitor for GC
// ============================================================================

/// distinct modes for the GC visitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitorKind {
    /// Full Major GC (Mark everything).
    Major,
    /// Minor GC (Mark only Young, stop at Old).
    Minor,
}

/// A concrete visitor struct used by the GC.
///
/// We use a single struct with a 'kind' field to handle both Major and Minor
/// GC passes. This allows `GcBox` to store a single function pointer for tracing
/// that takes `&mut GcVisitor`.
pub struct GcVisitor {
    /// The kind of collection being performed.
    pub kind: VisitorKind,
    /// Worklist for iterative tracing.
    ///
    /// NOTE: This uses an unbounded Vec. For very deep graphs with millions
    /// of objects, this could consume significant memory. Future optimization:
    /// use a chunked/overflow-resistant queue that spills to heap
    /// or uses multiple segments when approaching capacity limits.
    pub(crate) worklist: Vec<std::ptr::NonNull<crate::ptr::GcBox<()>>>,
    /// Count of objects marked during this collection.
    pub(crate) objects_marked: usize,
}

/// A visitor for concurrent/parallel garbage collection marking.
///
/// This visitor routes discovered references to the appropriate worker's
/// mark queue based on the target object's page ownership. This enables
/// efficient load balancing when objects in one thread's heap reference
/// objects in another thread's heap.
#[allow(dead_code)]
pub struct GcVisitorConcurrent<'a> {
    /// The kind of collection being performed.
    pub kind: VisitorKind,
    /// Reference to the shared page-to-worker mapping.
    /// Maps page addresses to worker queue indices for routing references.
    page_to_queue: &'a HashMap<usize, usize>,
    /// Thread ID for checking ownership.
    thread_id: u64,
    /// Callback for routing discovered references to work queues.
    route_fn: &'a mut dyn FnMut(*const GcBox<()>),
}

#[allow(dead_code)]
impl<'a> GcVisitorConcurrent<'a> {
    /// Create a new concurrent visitor.
    #[inline]
    #[must_use]
    pub fn new(
        kind: VisitorKind,
        page_to_queue: &'a HashMap<usize, usize>,
        route_fn: &'a mut dyn FnMut(*const GcBox<()>),
    ) -> Self {
        Self {
            kind,
            page_to_queue,
            thread_id: super::heap::get_thread_id(),
            route_fn,
        }
    }

    /// Route a Gc pointer to the appropriate worker's queue.
    ///
    /// If the target object is on a page owned by another thread, this
    /// routes the reference to that thread's work queue for efficient
    /// load balancing.
    fn route_reference<T: Trace>(&mut self, gc: &Gc<T>) {
        let raw = Gc::<T>::as_ptr(gc);
        if raw.is_null() {
            return;
        }

        // SAFETY: We verified raw is non-null above. The ptr_to_page_header
        // returns a valid NonNull<PageHeader> for any valid GC pointer.
        // We verify the magic number before accessing the header fields.
        // The object index is validated before marking to ensure we don't
        // access out-of-bounds bitmap bits.
        unsafe {
            let ptr_addr = raw.cast::<u8>();
            let header = super::heap::ptr_to_page_header(ptr_addr);

            if (*header.as_ptr()).magic != super::heap::MAGIC_GC_PAGE {
                return;
            }

            let page_addr = header.as_ptr() as usize;

            if let Some(idx) = super::heap::ptr_to_object_index(raw.cast()) {
                if self.kind == VisitorKind::Minor && (*header.as_ptr()).generation > 0 {
                    return;
                }

                if (*header.as_ptr()).is_marked(idx) {
                    return;
                }
                (*header.as_ptr()).set_mark(idx);
            } else {
                return;
            }

            let worker_idx = self.page_to_queue.get(&page_addr).copied();

            if let Some(_idx) = worker_idx {
                (self.route_fn)(raw.cast());
            }
        }
    }
}

#[allow(dead_code)]
impl Visitor for GcVisitorConcurrent<'_> {
    fn visit<T: Trace>(&mut self, gc: &Gc<T>) {
        self.route_reference(gc);
    }

    unsafe fn visit_region(&mut self, _ptr: *const u8, _len: usize) {
        // Conservative scanning would be implemented here
    }
}

// ============================================================================
// Trace implementations for Gc<T>
// ============================================================================

// SAFETY: Gc<T> traces its inner value
unsafe impl<T: Trace> Trace for Gc<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        visitor.visit(self);
    }
}

// ============================================================================
// Trace implementations for primitive types
// ============================================================================

macro_rules! impl_trace_for_primitives {
    ($($t:ty),* $(,)?) => {
        $(
            // SAFETY: Primitive types contain no Gc pointers
            unsafe impl Trace for $t {
                #[inline]
                fn trace(&self, _visitor: &mut impl Visitor) {}
            }
        )*
    };
}

impl_trace_for_primitives! {
    // Signed integers
    i8, i16, i32, i64, i128, isize,
    // Unsigned integers
    u8, u16, u32, u64, u128, usize,
    // Floating point
    f32, f64,
    // Other primitives
    bool, char, (),
    // String types
    String, str,
    // Common std types without Gc
    std::time::Duration,
    std::time::Instant,
    std::time::SystemTime,
    std::path::Path,
    std::path::PathBuf,
    std::ffi::OsStr,
    std::ffi::OsString,
    std::ffi::CStr,
    std::ffi::CString,
    std::net::IpAddr,
    std::net::Ipv4Addr,
    std::net::Ipv6Addr,
    std::net::SocketAddr,
    std::net::SocketAddrV4,
    std::net::SocketAddrV6,
    std::sync::atomic::AtomicBool,
    std::sync::atomic::AtomicU64,
    std::sync::atomic::AtomicIsize,
    std::sync::atomic::AtomicUsize,
}

// ============================================================================
// Trace implementations for std container types
// ============================================================================

// SAFETY: References trace their target
unsafe impl<T: Trace + ?Sized> Trace for &T {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        T::trace(self, visitor);
    }
}

// SAFETY: Mutable references trace their target
unsafe impl<T: Trace + ?Sized> Trace for &mut T {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        T::trace(self, visitor);
    }
}

// SAFETY: Box traces its contents
unsafe impl<T: Trace + ?Sized> Trace for Box<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        T::trace(self.as_ref(), visitor);
    }
}

// SAFETY: Rc traces its contents (but Rc itself should be avoided with Gc)
unsafe impl<T: Trace + ?Sized> Trace for Rc<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        T::trace(self.as_ref(), visitor);
    }
}

// SAFETY: Arc traces its contents
unsafe impl<T: Trace + ?Sized> Trace for Arc<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        T::trace(self.as_ref(), visitor);
    }
}

// SAFETY: Vec traces all elements
/// Additionally marks the Vec's storage buffer page as dirty so GC will scan it.
unsafe impl<T: Trace> Trace for Vec<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        // Trace all elements first
        for item in self {
            item.trace(visitor);
        }

        // Mark the Vec's storage buffer page as dirty
        // This ensures GC will scan this page to find Gc pointers
        if !self.is_empty() {
            unsafe {
                crate::heap::mark_page_dirty_for_ptr(self.as_ptr().cast::<u8>());
            }
        }
    }
}

// SAFETY: Arrays trace all elements
unsafe impl<T: Trace, const N: usize> Trace for [T; N] {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
        }
    }
}

// SAFETY: Slices trace all elements
unsafe impl<T: Trace> Trace for [T] {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
        }
    }
}

// SAFETY: Option traces its contents if Some
unsafe impl<T: Trace> Trace for Option<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        if let Some(inner) = self {
            inner.trace(visitor);
        }
    }
}

// SAFETY: Result traces both Ok and Err variants
unsafe impl<T: Trace, E: Trace> Trace for Result<T, E> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        match self {
            Ok(v) => v.trace(visitor),
            Err(e) => e.trace(visitor),
        }
    }
}

// SAFETY: Cell<T> traces its contents (requires Copy to get value)
unsafe impl<T: Trace + Copy> Trace for Cell<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        self.get().trace(visitor);
    }
}

// SAFETY: RefCell traces its contents
unsafe impl<T: Trace + ?Sized> Trace for RefCell<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        // Try to borrow; if already borrowed mutably, skip
        // (the borrower is responsible for tracing)
        if let Ok(inner) = self.try_borrow() {
            inner.trace(visitor);
        }
    }
}

// SAFETY: VecDeque traces all elements
/// Additionally marks the `VecDeque`'s storage buffer page as dirty so GC will scan it.
unsafe impl<T: Trace> Trace for VecDeque<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
        }
        // Mark the VecDeque's storage buffer page as dirty
        // This ensures GC will scan this page to find Gc pointers
        // VecDeque uses a ring buffer - get pointer to first element if any
        if !self.is_empty() {
            // SAFETY: VecDeque is non-empty, so there's at least one element
            let ptr = std::ptr::from_ref(self.front().unwrap()).cast::<u8>();
            unsafe {
                crate::heap::mark_page_dirty_for_ptr(ptr);
            }
        }
    }
}

// SAFETY: LinkedList traces all elements
unsafe impl<T: Trace> Trace for LinkedList<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
        }
    }
}

// SAFETY: HashMap traces all values (keys assumed to not contain Gc)
unsafe impl<K: Trace, V: Trace, S: BuildHasher> Trace for HashMap<K, V, S> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for (k, v) in self {
            k.trace(visitor);
            v.trace(visitor);
        }
    }
}

// SAFETY: HashSet traces all elements
unsafe impl<T: Trace, S: BuildHasher> Trace for HashSet<T, S> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
        }
    }
}

// SAFETY: BTreeMap traces all key-value pairs
unsafe impl<K: Trace, V: Trace> Trace for BTreeMap<K, V> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for (k, v) in self {
            k.trace(visitor);
            v.trace(visitor);
        }
    }
}

// SAFETY: BTreeSet traces all elements
unsafe impl<T: Trace> Trace for BTreeSet<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
        }
    }
}

// ============================================================================
// Trace implementations for tuples
// ============================================================================

macro_rules! impl_trace_for_tuples {
    () => {};
    ($first:ident $(, $rest:ident)*) => {
        // SAFETY: Tuples trace all their elements
        unsafe impl<$first: Trace $(, $rest: Trace)*> Trace for ($first, $($rest,)*) {
            #[inline]
            #[allow(non_snake_case)]
            fn trace(&self, visitor: &mut impl Visitor) {
                let ($first, $($rest,)*) = self;
                $first.trace(visitor);
                $($rest.trace(visitor);)*
            }
        }
        impl_trace_for_tuples!($($rest),*);
    };
}

impl_trace_for_tuples!(A, B, C, D, E, F, G, H, I, J, K, L);

// ============================================================================
// Trace implementation for PhantomData
// ============================================================================

// SAFETY: PhantomData contains no actual data
unsafe impl<T: ?Sized> Trace for std::marker::PhantomData<T> {
    #[inline]
    fn trace(&self, _visitor: &mut impl Visitor) {}
}

// ============================================================================
// Trace implementation for NonZero types
// ============================================================================

// SAFETY: NonZero types are just wrappers around primitives
unsafe impl Trace for std::num::NonZeroU8 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroU16 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroU32 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroU64 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroU128 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroUsize {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroI8 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroI16 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroI32 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroI64 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroI128 {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}
unsafe impl Trace for std::num::NonZeroIsize {
    fn trace(&self, _visitor: &mut impl Visitor) {}
}

#[cfg(feature = "test-util")]
pub mod test_util {
    /// Creates a static `Trace` implementation for types that cannot contain `Gc` pointers.
    ///
    /// This macro is used for external reference tracking tests. It provides a no-op `Trace`
    /// implementation for types that are `'static` and guaranteed not to contain any `Gc` pointers.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use rudo_gc::static_collect;
    ///
    /// struct MyType;
    /// static_collect!(MyType);
    /// ```
    #[macro_export]
    macro_rules! static_collect {
        (<$($params:tt),+ $(,)*> $type:ty $(where $($bounds:tt)+)?) => {
            unsafe impl<'gc, $($params),*> $crate::Trace for $type
            where
                $type: 'static,
                $($($bounds)+)*
            {
                fn trace(&self, _visitor: &mut impl $crate::Visitor) {}
            }
        };
        ($type:ty) => {
            unsafe impl<'gc> $crate::Trace for $type
            where
                $type: 'static,
            {
                fn trace(&self, _visitor: &mut impl $crate::Visitor) {}
            }
        }
    }
}
