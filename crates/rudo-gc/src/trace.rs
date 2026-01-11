//! Trace trait and Visitor pattern for garbage collection.
//!
//! Types that implement `Trace` can be stored in `Gc<T>` and will be
//! automatically traversed during garbage collection.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, LinkedList, VecDeque};
use std::hash::BuildHasher;
use std::rc::Rc;
use std::sync::Arc;

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
    fn visit<T: Trace + ?Sized>(&mut self, gc: &Gc<T>);
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
    /// Current thread ID for parallel GC ownership checking.
    pub thread_id: usize,
    /// Reference to the thread registry for forwarding to remote threads.
    /// None when running in single-threaded mode.
    pub registry: Option<std::sync::Arc<std::sync::Mutex<crate::heap::ThreadRegistry>>>,
}

impl GcVisitor {
    /// Create a new GcVisitor for single-threaded GC.
    pub fn new(kind: VisitorKind) -> Self {
        Self {
            kind,
            thread_id: 0,
            registry: None,
        }
    }

    /// Create a new GcVisitor for parallel GC.
    pub fn new_parallel(
        kind: VisitorKind,
        thread_id: usize,
        registry: std::sync::Arc<std::sync::Mutex<crate::heap::ThreadRegistry>>,
    ) -> Self {
        Self {
            kind,
            thread_id,
            registry: Some(registry),
        }
    }

    /// Visit a GC pointer with Remote Mentions support.
    ///
    /// If the pointer belongs to the current thread's pages, mark it directly.
    /// Otherwise, forward it to the owning thread's remote inbox.
    pub fn visit_with_ownership<T: Trace + ?Sized>(&mut self, gc: &Gc<T>) {
        if let Some(ptr) = gc.raw_ptr().as_option() {
            let gc_ptr = crate::ptr::Gc::internal_ptr(gc) as *const u8;

            // Check if this is a valid GC pointer
            if !unsafe { crate::heap::is_gc_pointer(gc_ptr) } {
                return;
            }

            // Get the page owner
            let owner_id = unsafe { crate::heap::ptr_to_page_owner(gc_ptr) };

            if owner_id == self.thread_id {
                // Local object - mark directly
                unsafe {
                    if self.kind == VisitorKind::Minor {
                        super::gc::mark_object_minor(ptr.cast(), self);
                    } else {
                        super::gc::mark_object(ptr.cast(), self);
                    }
                }
            } else {
                // Remote object - forward to owner's inbox
                self.forward_to_remote(gc_ptr, owner_id);
            }
        }
    }

    /// Forward a pointer to a remote thread's inbox.
    fn forward_to_remote(&self, ptr: *const u8, owner_id: usize) {
        if let Some(ref registry) = self.registry {
            let registry = registry.lock().unwrap();
            if owner_id < registry.threads.len() {
                let owner_tcb = &registry.threads[owner_id];
                owner_tcb.push_remote_inbox(ptr);
                owner_tcb.record_remote_sent();
            }
        }
    }
}

// ============================================================================
// Trace implementations for Gc<T>
// ============================================================================

// SAFETY: Gc<T> traces its inner value
unsafe impl<T: Trace + ?Sized> Trace for Gc<T> {
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
unsafe impl<T: Trace> Trace for Vec<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
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
unsafe impl<T: Trace> Trace for VecDeque<T> {
    #[inline]
    fn trace(&self, visitor: &mut impl Visitor) {
        for item in self {
            item.trace(visitor);
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
