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
