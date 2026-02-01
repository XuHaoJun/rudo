//! `HandleScope` v2 implementation for compile-time safe GC handles.
//!
//! This module provides lifetime-bound handles that prevent dangling references
//! at compile time, following V8's `HandleScope` design patterns.
//!
//! # Overview
//!
//! A `HandleScope` creates a scope for GC handles. When the scope is dropped,
//! all handles created within it are automatically invalidated. This ensures
//! that handles can never outlive the objects they reference.
//!
//! ## Handle Types
//!
//! - [`HandleScope`] - Basic scope for creating handles with compile-time lifetime binding
//! - [`Handle`] - A GC reference bound to a specific scope's lifetime
//! - [`EscapeableHandleScope`] - Allows handles to escape to an outer scope
//! - [`MaybeHandle`] - Optional handle pattern for nullable GC references
//! - [`SealedHandleScope`] - Debug-only scope that prevents handle creation
//!
//! # Example
//!
//! ```
//! use rudo_gc::{Gc, Trace};
//!
//! #[derive(Trace, Debug)]
//! struct Data {
//!     value: i32,
//! }
//!
//! fn example<T: Trace>(gc: &Gc<Data>) {
//!     // Create a handle scope
//!     let scope = rudo_gc::handles::HandleScope::new(
//!         &rudo_gc::heap::current_thread_control_block().unwrap()
//!     );
//!
//!     // Create a handle bound to this scope
//!     let handle = scope.handle(gc);
//!
//!     // Use the handle (it cannot outlive the scope)
//!     println!("Value: {}", handle.value);
//! } // Handle and scope are dropped here
//! ```
//!
//! # Thread Safety
//!
//! - `HandleScope` and `Handle` are `!Send + !Sync` (thread-local only)
//! - `EscapeableHandleScope`, `MaybeHandle`, `SealedHandleScope` are `!Send + !Sync`
//! - All handle types implement `Copy` and `Clone` where appropriate

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::ptr_as_ptr)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::elidable_lifetime_names)]
#![allow(clippy::explicit_auto_deref)]
#![allow(clippy::ref_as_ptr)]
#![allow(clippy::manual_assert)]

mod r#async;
mod local_handles;

#[cfg(test)]
mod tests;

pub use local_handles::{
    HandleBlock, HandleScopeData, HandleSlot, LocalHandles, HANDLE_BLOCK_SIZE,
};
pub use r#async::{
    AsyncHandle, AsyncHandleGuard, AsyncHandleScope, AsyncScopeData, AsyncScopeEntry,
};

use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Deref;

use crate::heap::ThreadControlBlock;
use crate::ptr::GcBox;
use crate::trace::Trace;
use crate::Gc;

/// A scope for GC handles with compile-time lifetime binding.
///
/// `HandleScope` establishes a scope where handles can be created.
/// All handles created within the scope are bound to its lifetime and
/// become invalid when the scope is dropped.
///
/// # Level System
///
/// `HandleScope`s use a level counter to track nesting depth. Each new
/// scope increments the level, and dropping restores the previous level.
/// This allows the GC to efficiently identify handles belonging to each scope.
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::handles::{HandleScope, Handle};
///
/// #[derive(Trace)]
/// struct MyData { value: i32 }
///
/// fn process_data<T: Trace>(gc: &Gc<MyData>) {
///     // Create a handle scope
///     let scope = HandleScope::new(
///         &rudo_gc::heap::current_thread_control_block().unwrap()
///     );
///
///     // Create handles within the scope
///     let handle = scope.handle(gc);
///     println!("{}", handle.value);
/// }
/// ```
pub struct HandleScope<'env> {
    tcb: &'env ThreadControlBlock,
    prev_next: *mut HandleSlot,
    prev_limit: *mut HandleSlot,
    prev_level: u32,
    _marker: PhantomData<*mut ()>,
}

impl<'env> HandleScope<'env> {
    /// Creates a new `HandleScope` for the given thread control block.
    ///
    /// # Arguments
    ///
    /// * `tcb` - The thread control block for the current thread
    ///
    /// # Returns
    ///
    /// A new `HandleScope` with level incremented by 1
    ///
    /// # Panics
    ///
    /// Panics if the handle scope level overflows (typically indicates a bug)
    #[inline]
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        let local_handles = tcb.local_handles_ptr();

        // SAFETY: We have exclusive access via the borrow of tcb
        let (prev_next, prev_limit, prev_level) = unsafe {
            let handles = &mut *local_handles;
            let scope_data = handles.scope_data_mut();
            let prev_next = scope_data.next;
            let prev_limit = scope_data.limit;
            let prev_level = scope_data.level;
            scope_data.level = prev_level
                .checked_add(1)
                .expect("HandleScope level overflow");
            (prev_next, prev_limit, prev_level)
        };

        Self {
            tcb,
            prev_next,
            prev_limit,
            prev_level,
            _marker: PhantomData,
        }
    }

    /// Creates a `Handle` to the given GC object within this scope.
    ///
    /// The returned handle is bound to this scope's lifetime and will
    /// become invalid when the scope is dropped.
    ///
    /// # Arguments
    ///
    /// * `gc` - The GC object to create a handle for
    ///
    /// # Returns
    ///
    /// A `Handle<'scope, T>` that dereferences to `&T`
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// fn example() {
    ///     let gc = Gc::new(Data { value: 42 });
    ///     let scope = rudo_gc::handles::HandleScope::new(
    ///         &rudo_gc::heap::current_thread_control_block().unwrap()
    ///     );
    ///     let handle = scope.handle(&gc);
    ///     assert_eq!(handle.value, 42);
    /// }
    /// ```
    #[inline]
    pub fn handle<'scope, T: Trace>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T> {
        let local_handles = self.tcb.local_handles_ptr();

        // SAFETY: We have exclusive access via the borrow of tcb
        let slot = unsafe { (*local_handles).allocate() };

        let gc_box_ptr = Gc::internal_ptr(gc) as *const GcBox<()>;
        unsafe {
            (*slot).set(gc_box_ptr);
        }

        Handle {
            slot,
            _marker: PhantomData,
        }
    }

    /// Returns the current nesting level of this scope.
    ///
    /// The root scope has level 1, and each nested scope increments by 1.
    /// This is primarily useful for debugging and testing.
    ///
    /// # Returns
    ///
    /// The current scope level
    #[inline]
    pub fn level(&self) -> u32 {
        unsafe { (*self.tcb.local_handles_ptr()).scope_data().level }
    }
}

impl Drop for HandleScope<'_> {
    fn drop(&mut self) {
        let local_handles = self.tcb.local_handles_ptr();

        // SAFETY: We have exclusive access via the borrow of tcb
        unsafe {
            let handles = &mut *local_handles;
            let scope_data = handles.scope_data_mut();
            scope_data.next = self.prev_next;
            scope_data.limit = self.prev_limit;
            scope_data.level = self.prev_level;
        }
    }
}

/// A GC reference bound to a specific scope's lifetime.
///
/// `Handle` provides safe access to GC-allocated objects within a handle scope.
/// The handle's lifetime is tied to the scope it was created in, preventing
/// use-after-free bugs at compile time.
///
/// # Deref
///
/// `Handle` implements `Deref<Target = T>`, allowing transparent access to
/// the underlying data:
///
/// ```
/// let handle: Handle<'_, MyData>;
/// let value: &MyData = &*handle; // Via Deref
/// let direct: &MyData = handle.get(); // Direct call
/// ```
///
/// # Copy and Clone
///
/// `Handle` implements `Copy`, so handles can be freely copied without
/// explicit cloning:
///
/// ```
/// let handle1 = scope.handle(&gc);
/// let handle2 = handle1; // Copy, not clone
/// ```
pub struct Handle<'scope, T: Trace + 'static> {
    slot: *const HandleSlot,
    _marker: PhantomData<(&'scope (), *const T)>,
}

impl<'scope, T: Trace + 'static> Handle<'scope, T> {
    /// Returns a reference to the underlying data.
    ///
    /// # Returns
    ///
    /// A reference to the GC-allocated data
    ///
    /// # Safety
    ///
    /// The handle must be valid (the scope it belongs to must not have been dropped).
    /// This is guaranteed by the lifetime system when used correctly.
    #[inline]
    pub fn get(&self) -> &T {
        unsafe {
            let slot = &*self.slot;
            let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
            let gc_box = &*gc_box_ptr;
            gc_box.value()
        }
    }

    /// Converts this handle into a `Gc<T>`.
    ///
    /// This increments the reference count, allowing the handle's value
    /// to outlive the handle scope.
    ///
    /// # Returns
    ///
    /// A new `Gc<T>` pointing to the same data
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// fn escape_example() {
    ///     let gc = Gc::new(Data { value: 42 });
    ///     let scope = rudo_gc::handles::HandleScope::new(
    ///         &rudo_gc::heap::current_thread_control_block().unwrap()
    ///     );
    ///     let handle = scope.handle(&gc);
    ///
    ///     // Escape the handle to a Gc that outlives the scope
    ///     let escaped_gc = handle.to_gc();
    ///     drop(scope);
    ///     // escaped_gc is still valid!
    /// }
    /// ```
    #[inline]
    pub fn to_gc(&self) -> Gc<T> {
        unsafe {
            let slot = &*self.slot;
            let ptr = slot.as_ptr() as *const u8;
            // SAFETY: The handle slot contains a valid GcBox pointer
            // This increments the reference count
            let gc: Gc<T> = Gc::from_raw(ptr);
            // Increment ref count since from_raw doesn't
            let gc_clone = gc.clone();
            std::mem::forget(gc);
            gc_clone
        }
    }

    /// Returns a raw pointer to the underlying `GcBox`.
    ///
    /// # Returns
    ///
    /// A raw pointer to the `GcBox<T>`
    ///
    /// # Safety
    ///
    /// The caller must ensure the handle is valid and the pointer
    /// is only used while the handle scope is active.
    #[inline]
    pub unsafe fn as_ptr(&self) -> *const GcBox<T> {
        let slot = unsafe { &*self.slot };
        slot.as_ptr() as *const GcBox<T>
    }
}

impl<T: Trace + 'static> Deref for Handle<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<T: Trace + 'static> Clone for Handle<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Trace + 'static> Copy for Handle<'_, T> {}

impl<T: Trace + 'static + std::fmt::Debug> std::fmt::Debug for Handle<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Handle").field(&self.get()).finish()
    }
}

impl<T: Trace + 'static + std::fmt::Display> std::fmt::Display for Handle<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.get(), f)
    }
}

/// A handle scope that allows handles to escape to an outer scope.
///
/// `EscapeableHandleScope` extends `HandleScope` with the ability to
/// "escape" handles to an outer scope. This is useful for patterns where
/// a handle needs to outlive the scope it was created in.
///
/// # Single-Use Constraint
///
/// The `escape()` method can only be called once per `EscapeableHandleScope`.
/// Attempting to escape a second time will panic.
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::handles::{HandleScope, EscapeableHandleScope, Handle};
///
/// #[derive(Trace)]
/// struct Node { value: i32 }
///
/// fn create_and_escape() -> Handle<'static, Node> {
///     let outer = HandleScope::new(
///         &rudo_gc::heap::current_thread_control_block().unwrap()
///     );
///
///     let escaped = {
///         let escape_scope = EscapeableHandleScope::new(
///             &rudo_gc::heap::current_thread_control_block().unwrap()
///         );
///         let gc = Gc::new(Node { value: 42 });
///         let handle = escape_scope.handle(&gc);
///         escape_scope.escape(&outer, handle)
///     };
///
///     escaped // outlives the inner scope
/// }
/// ```
pub struct EscapeableHandleScope<'env> {
    inner: HandleScope<'env>,
    escaped: Cell<bool>,
    escape_slot: *mut HandleSlot,
    #[cfg(debug_assertions)]
    #[allow(dead_code)]
    parent_level: u32,
}

impl<'env> EscapeableHandleScope<'env> {
    /// Creates a new `EscapeableHandleScope`.
    ///
    /// Pre-allocates an escape slot in the parent scope for the single
    /// handle that can be escaped.
    ///
    /// # Arguments
    ///
    /// * `tcb` - The thread control block for the current thread
    #[inline]
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        let local_handles = tcb.local_handles_ptr();

        // Pre-allocate escape slot in parent scope
        let escape_slot = unsafe { (*local_handles).allocate() };

        #[cfg(debug_assertions)]
        let parent_level = unsafe { (*local_handles).scope_data().level };

        let inner = HandleScope::new(tcb);

        Self {
            inner,
            escaped: Cell::new(false),
            escape_slot,
            #[cfg(debug_assertions)]
            parent_level,
        }
    }

    /// Creates a handle within this scope.
    ///
    /// # Arguments
    ///
    /// * `gc` - The GC object to create a handle for
    ///
    /// # Returns
    ///
    /// A handle bound to this scope
    #[inline]
    pub fn handle<'scope, T: Trace + 'static>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T> {
        self.inner.handle(gc)
    }

    /// Escapes a handle to the parent scope.
    ///
    /// The handle returned is bound to the parent scope's lifetime
    /// and can be used after this `EscapeableHandleScope` is dropped.
    ///
    /// # Arguments
    ///
    /// * `parent` - The parent scope to escape to
    /// * `handle` - The handle to escape
    ///
    /// # Returns
    ///
    /// A handle bound to the parent scope
    ///
    /// # Panics
    ///
    /// - Panics if `escape()` has already been called on this scope
    /// - Panics in debug mode if the parent scope level doesn't match
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::{HandleScope, EscapeableHandleScope};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// fn escape_to_outer() {
    ///     let outer = HandleScope::new(
    ///         &rudo_gc::heap::current_thread_control_block().unwrap()
    ///     );
    ///
    ///     let escaped_handle = {
    ///         let escape_scope = EscapeableHandleScope::new(
    ///             &rudo_gc::heap::current_thread_control_block().unwrap()
    ///         );
    ///         let gc = Gc::new(Data { value: 123 });
    ///         let inner_handle = escape_scope.handle(&gc);
    ///         escape_scope.escape(&outer, inner_handle)
    ///     };
    ///
    ///     // escaped_handle is valid here, even though escape_scope is dropped
    ///     assert_eq!(escaped_handle.value, 123);
    /// }
    /// ```
    #[inline]
    pub fn escape<'parent, T: Trace + 'static>(
        &self,
        _parent: &'parent HandleScope<'_>,
        handle: Handle<'_, T>,
    ) -> Handle<'parent, T> {
        if self.escaped.get() {
            panic!("EscapeableHandleScope::escape() can only be called once");
        }

        #[cfg(debug_assertions)]
        {
            if self.parent_level + 1 != self.inner.level() {
                panic!("escape() called with incorrect parent scope");
            }
        }

        self.escaped.set(true);

        unsafe {
            let src_slot = &*handle.slot;
            (*self.escape_slot).set(src_slot.as_ptr());
        }

        Handle {
            slot: self.escape_slot,
            _marker: PhantomData,
        }
    }
}

/// An optional handle pattern for nullable GC references.
///
/// `MaybeHandle` represents a handle that may or may not exist.
/// This is useful for APIs that need to distinguish between "no handle"
/// and "invalid handle" cases.
///
/// # Example
///
/// ```
/// use rudo_gc::handles::{Handle, MaybeHandle};
///
/// fn try_lookup<T>(maybe: MaybeHandle<'_, T>) -> Option<Handle<'_, T>> {
///     maybe.to_handle()
/// }
///
/// // Returns None for empty handle
/// let empty = MaybeHandle::<i32>::empty();
/// assert!(try_lookup(empty).is_none());
/// ```
pub struct MaybeHandle<'scope, T: Trace + 'static> {
    slot: *const HandleSlot,
    _marker: PhantomData<(&'scope (), *const T)>,
}

impl<'scope, T: Trace + 'static> MaybeHandle<'scope, T> {
    /// Creates an empty `MaybeHandle`.
    ///
    /// # Returns
    ///
    /// A handle that represents "no handle"
    #[inline]
    pub const fn empty() -> Self {
        Self {
            slot: std::ptr::null(),
            _marker: PhantomData,
        }
    }

    /// Creates a `MaybeHandle` from an existing `Handle`.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle to wrap
    ///
    /// # Returns
    ///
    /// A `MaybeHandle` containing the handle
    #[inline]
    pub fn from_handle(handle: Handle<'scope, T>) -> Self {
        Self {
            slot: handle.slot,
            _marker: PhantomData,
        }
    }

    /// Returns `true` if this is an empty handle.
    ///
    /// # Returns
    ///
    /// `true` if no handle exists, `false` otherwise
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.slot.is_null()
    }

    /// Converts this `MaybeHandle` into an `Option<Handle>`.
    ///
    /// # Returns
    ///
    /// `Some(handle)` if a handle exists, `None` otherwise
    #[inline]
    pub fn to_handle(self) -> Option<Handle<'scope, T>> {
        if self.slot.is_null() {
            None
        } else {
            Some(Handle {
                slot: self.slot,
                _marker: PhantomData,
            })
        }
    }
}

impl<T: Trace + 'static> Clone for MaybeHandle<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Trace + 'static> Copy for MaybeHandle<'_, T> {}

impl<T: Trace + 'static> Default for MaybeHandle<'_, T> {
    fn default() -> Self {
        Self::empty()
    }
}

/// A debug-only scope that prevents handle creation.
///
/// `SealedHandleScope` is used in debug builds to prevent handle
/// allocation in critical sections. In release builds, it's a no-op.
///
/// # Use Case
///
/// Use `SealedHandleScope` when you need to ensure no handles are
/// being created during some operation (e.g., during GC marking):
///
/// ```ignore
/// fn perform_gc_operation() {
///     let _sealed = SealedHandleScope::new(tcb);
///     // Any attempt to create a handle here will panic in debug mode
///     perform_operation();
/// }
/// ```
///
/// # Debug vs Release
///
/// - **Debug**: Prevents handle creation, panics if attempted
/// - **Release**: No effect, handles can be freely created
#[cfg(debug_assertions)]
pub struct SealedHandleScope<'env> {
    tcb: &'env ThreadControlBlock,
    prev_sealed_level: u32,
}

#[cfg(not(debug_assertions))]
pub struct SealedHandleScope<'env>(PhantomData<&'env ()>);

impl<'env> SealedHandleScope<'env> {
    /// Creates a new `SealedHandleScope`.
    ///
    /// In debug mode, records the current sealed level to restore on drop.
    /// In release mode, does nothing.
    ///
    /// # Arguments
    ///
    /// * `tcb` - The thread control block for the current thread
    #[cfg(debug_assertions)]
    #[inline]
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        let local_handles = tcb.local_handles_ptr();

        let prev_sealed_level = unsafe {
            let handles = &mut *local_handles;
            let scope_data = handles.scope_data_mut();
            let prev = scope_data.sealed_level;
            scope_data.sealed_level = scope_data.level;
            prev
        };

        Self {
            tcb,
            prev_sealed_level,
        }
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    pub fn new(_tcb: &'env ThreadControlBlock) -> Self {
        Self(PhantomData)
    }
}

#[cfg(debug_assertions)]
impl Drop for SealedHandleScope<'_> {
    fn drop(&mut self) {
        let local_handles = self.tcb.local_handles_ptr();

        unsafe {
            let handles = &mut *local_handles;
            handles.scope_data_mut().sealed_level = self.prev_sealed_level;
        }
    }
}
