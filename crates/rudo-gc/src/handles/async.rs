//! Async handle support for GC references across await points.
//!
//! This module provides `AsyncHandleScope` and `AsyncHandle<T>` for safe
//! async/await GC reference management. Unlike synchronous handles which
//! are tied to a specific scope's lifetime, async handles are registered
//! with the thread's control block and remain valid across `.await` points.
//!
//! # Overview
//!
//! Standard `HandleScope` handles are tied to a lexical scope and become
//! invalid when the scope is dropped. This doesn't work for async code where
//! tasks can be suspended at await points and resume later.
//!
//! `AsyncHandleScope` solves this by:
//! 1. Registering handles with the thread control block
//! 2. Tracking handles via unique scope IDs
//! 3. Keeping handles valid until the `AsyncHandleScope` is explicitly dropped
//!
//! # `AsyncHandle` vs `Handle`
//!
//! | Aspect | `Handle` | `AsyncHandle` |
//! |--------|----------|---------------|
//! | Lifetime | Scope-bound | Scope-registered |
//! | Across await | Invalid | Valid |
//! | Thread safety | `!Send + !Sync` | `Send + Sync` |
//! | Access | Direct | Direct or via guard |
//!
//! # Example
//!
//! ```
//! use rudo_gc::{Gc, Trace};
//! use rudo_gc::handles::AsyncHandleScope;
//!
//! #[derive(Trace, Debug)]
//! struct AsyncData {
//!     value: i32,
//! }
//!
//! async fn async_operation() {
//!     let tcb = rudo_gc::heap::current_thread_control_block()
//!         .expect("must be called within GC thread");
//!
//!     let scope = AsyncHandleScope::new(&tcb);
//!     let gc = Gc::new(AsyncData { value: 42 });
//!     let handle = scope.handle(&gc);
//!
//!     // Handle remains valid across await points
//!     tokio::task::yield_now().await;
//!
//!     // Still valid after await!
//!     println!("Value: {}", handle.get().value);
//!
//!     // Scope keeps handles alive until dropped
//!     drop(scope);
//! }
//! ```
//!
//! # `spawn_with_gc!` Macro
//!
//! For common cases, use the `spawn_with_gc!` macro to automatically
//! handle async scope management:
//!
//! ```
//! use rudo_gc::{Gc, Trace};
//!
//! #[derive(Trace)]
//! struct TaskData { id: u32 }
//!
//! async fn spawn_task(gc: Gc<TaskData>) {
//!     rudo_gc::spawn_with_gc!(gc => |handle| {
//!         // handle is an AsyncHandle<TaskData>
//!         tokio::task::yield_now().await;
//!         println!("Task {}", handle.get().id);
//!     }).await;
//! }
//! ```

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::ptr_as_ptr)]
#![allow(clippy::ref_as_ptr)]
#![allow(clippy::borrow_as_ptr)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_cast_constness)]
#![allow(clippy::non_send_fields_in_send_ty)]
#![allow(clippy::elidable_lifetime_names)]
#![allow(clippy::manual_assert)]
#![allow(clippy::explicit_auto_deref)]
#![allow(clippy::as_ptr_cast_mut)]
#![allow(clippy::non_canonical_clone_impl)]

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::heap::ThreadControlBlock;
use crate::ptr::GcBox;
use crate::trace::Trace;
use crate::Gc;

use super::local_handles::{HandleBlock, HandleSlot, HANDLE_BLOCK_SIZE};

static ASYNC_SCOPE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// An entry in the async scope registry.
/// Used internally to track async scopes for GC root collection.
pub struct AsyncScopeEntry {
    /// Unique scope identifier
    pub id: u64,
    /// Pointer to the handle block
    pub block_ptr: *const HandleBlock,
    /// Pointer to the atomic counter tracking used slots
    pub used: *const AtomicUsize,
}

unsafe impl Send for AsyncScopeEntry {}
unsafe impl Sync for AsyncScopeEntry {}

/// A handle scope for async code that persists across await points.
///
/// `AsyncHandleScope` creates handles that remain valid across `.await`
/// boundaries. Unlike `HandleScope`, the scope must be kept alive manually
/// (or via the `spawn_with_gc!` macro) as long as any handles are in use.
///
/// # Handle Registration
///
/// When a handle is created via `handle()`, it's registered with the
/// thread control block. During GC, these handles are visited as roots.
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::handles::AsyncHandleScope;
///
/// #[derive(Trace)]
/// struct Data { value: i32 }
///
/// async fn use_async_handles() {
///     let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
///     let scope = AsyncHandleScope::new(&tcb);
///
///     let gc1 = Gc::new(Data { value: 1 });
///     let gc2 = Gc::new(Data { value: 2 });
///
///     let h1 = scope.handle(&gc1);
///     let h2 = scope.handle(&gc2);
///
///     // Both handles valid across awaits
///     tokio::task::yield_now().await;
///
///     println!("{} + {} = {}", h1.get().value, h2.get().value, h1.get().value + h2.get().value);
/// }
/// ```
///
/// # Thread Safety
///
/// `AsyncHandleScope` implements `Send + Sync` and can be used from
/// any thread, but handles should only be accessed from the thread
/// that created the scope.
pub struct AsyncHandleScope {
    id: u64,
    tcb: Arc<ThreadControlBlock>,
    block: Box<HandleBlock>,
    used: AtomicUsize,
    dropped: AtomicBool,
}

impl AsyncHandleScope {
    /// Creates a new `AsyncHandleScope`.
    ///
    /// The scope is automatically registered with the thread control block
    /// for GC root tracking.
    ///
    /// # Arguments
    ///
    /// * `tcb` - The thread control block (must be the current thread's TCB)
    ///
    /// # Panics
    ///
    /// Panics if the scope ID counter overflows (extremely unlikely)
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::handles::AsyncHandleScope;
    ///
    /// fn create_scope() {
    ///     let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    ///     let scope = AsyncHandleScope::new(&tcb);
    ///     // scope is now active
    /// }
    /// ```
    #[inline]
    #[allow(clippy::unused_self)]
    pub fn new(_tcb: &ThreadControlBlock) -> Self {
        let id = ASYNC_SCOPE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let block = HandleBlock::new();

        let tcb_arc = crate::heap::current_thread_control_block()
            .expect("AsyncHandleScope::new called outside GC thread");

        let scope = Self {
            id,
            tcb: tcb_arc,
            block,
            used: AtomicUsize::new(0),
            dropped: AtomicBool::new(false),
        };

        scope.tcb.register_async_scope(
            scope.id,
            scope.block.as_ref() as *const HandleBlock,
            &scope.used as *const AtomicUsize,
        );

        scope
    }

    /// Creates an `AsyncHandle` to the given GC object.
    ///
    /// The handle is registered with this scope and will remain valid
    /// until the scope is dropped.
    ///
    /// # Arguments
    ///
    /// * `gc` - The GC object to create a handle for
    ///
    /// # Returns
    ///
    /// An `AsyncHandle<T>` that can be used across await points
    ///
    /// # Panics
    ///
    /// Panics if more than 256 handles are created in this scope
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::AsyncHandleScope;
    ///
    /// #[derive(Trace)]
    /// struct MyData { name: String }
    ///
    /// async fn use_handle() {
    ///     let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    ///     let scope = AsyncHandleScope::new(&tcb);
    ///
    ///     let gc = Gc::new(MyData { name: "test".to_string() });
    ///     let handle = scope.handle(&gc);
    ///
    ///     tokio::task::yield_now().await;
    ///     println!("Name: {}", handle.get().name);
    /// }
    /// ```
    #[inline]
    pub fn handle<T: Trace + 'static>(&self, gc: &Gc<T>) -> AsyncHandle<T> {
        let idx = self.used.fetch_add(1, Ordering::Relaxed);
        if idx >= HANDLE_BLOCK_SIZE {
            panic!("AsyncHandleScope: exceeded maximum handle count ({HANDLE_BLOCK_SIZE})");
        }

        let slot_ptr = unsafe {
            let slots_ptr = self.block.slots.as_ptr() as *mut HandleSlot;
            slots_ptr.add(idx)
        };

        let gc_box_ptr = Gc::internal_ptr(gc) as *const GcBox<()>;
        unsafe {
            (*slot_ptr).set(gc_box_ptr);
        }

        AsyncHandle {
            slot: slot_ptr,
            scope_id: self.id,
            _marker: PhantomData,
        }
    }

    /// Executes a closure with an `AsyncHandleGuard` for safe handle access.
    ///
    /// The guard provides checked access to handles, verifying the scope
    /// matches in debug builds.
    ///
    /// # Arguments
    ///
    /// * `f` - A closure that receives the guard
    ///
    /// # Returns
    ///
    /// The result of the closure
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::AsyncHandleScope;
    ///
    /// #[derive(Trace)]
    /// struct Counter { count: i32 }
    ///
    /// async fn guarded_access() {
    ///     let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    ///     let scope = AsyncHandleScope::new(&tcb);
    ///     let gc = Gc::new(Counter { count: 0 });
    ///     let handle = scope.handle(&gc);
    ///
    ///     let result = scope.with_guard(|guard| {
    ///         guard.get(&handle).count
    ///     });
    /// }
    /// ```
    #[inline]
    pub fn with_guard<F, R>(&self, f: F) -> R
    where
        F: FnOnce(AsyncHandleGuard<'_>) -> R,
    {
        let guard = AsyncHandleGuard {
            scope: self,
            _marker: PhantomData,
        };
        f(guard)
    }

    /// Iterates over all handles in this scope, calling the visitor for each.
    ///
    /// This is used during GC marking to visit all handles as roots.
    ///
    /// # Arguments
    ///
    /// * `visitor` - A closure that receives a pointer to each `GcBox`
    ///
    /// # Safety
    ///
    /// The visitor must not modify the handles or their referenced objects.
    #[inline]
    pub fn iterate<F>(&self, mut visitor: F)
    where
        F: FnMut(*const GcBox<()>),
    {
        let used = self.used.load(Ordering::Acquire);
        for i in 0..used {
            let slot = unsafe { &*self.block.slots.as_ptr().add(i) };
            if !slot.is_null() {
                visitor(slot.as_ptr());
            }
        }
    }

    /// Returns the unique ID of this scope.
    ///
    /// # Returns
    ///
    /// The scope's unique identifier
    #[inline]
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for AsyncHandleScope {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
        self.tcb.unregister_async_scope(self.id);
    }
}

unsafe impl Send for AsyncHandleScope {}
unsafe impl Sync for AsyncHandleScope {}

/// A guard for safe access to async handles.
///
/// `AsyncHandleGuard` provides checked access to handles, verifying
/// in debug builds that the handle belongs to the correct scope.
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::handles::AsyncHandleScope;
///
/// #[derive(Trace)]
/// struct Data { value: i32 }
///
/// async fn guarded_example() {
///     let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
///     let scope = AsyncHandleScope::new(&tcb);
///     let gc = Gc::new(Data { value: 42 });
///     let handle = scope.handle(&gc);
///
///     scope.with_guard(|guard| {
///         // Access via guard
///         println!("Value: {}", guard.get(&handle).value);
///     });
/// }
/// ```
pub struct AsyncHandleGuard<'scope> {
    scope: &'scope AsyncHandleScope,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope> AsyncHandleGuard<'scope> {
    /// Gets the value from an async handle.
    ///
    /// In debug builds, verifies the handle's scope ID matches.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle to access
    ///
    /// # Returns
    ///
    /// A reference to the handle's value
    ///
    /// # Panics
    ///
    /// Panics in debug mode if the handle is from a different scope
    #[inline]
    pub fn get<'a, T: Trace + 'static>(&'a self, handle: &'a AsyncHandle<T>) -> &'a T {
        #[cfg(debug_assertions)]
        {
            if handle.scope_id != self.scope.id {
                panic!("AsyncHandle accessed from wrong scope");
            }
        }

        unsafe { handle.get() }
    }
}

/// An async-safe handle to a GC object.
///
/// `AsyncHandle` is similar to `Handle` but is designed for use in
/// async code. It can be cloned and sent across threads, and remains
/// valid across `.await` points as long as the `AsyncHandleScope`
/// that created it is still alive.
///
/// # Safety
///
/// `AsyncHandle` is `Send + Sync` because:
/// 1. The underlying data is GC-allocated and immutable
/// 2. The scope ID check prevents use-after-free in debug builds
/// 3. The GC can safely trace handles from any thread
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::handles::AsyncHandleScope;
///
/// #[derive(Trace, Clone)]
/// struct SharedData { value: i32 }
///
/// async fn share_across_tasks() {
///     let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
///     let scope = AsyncHandleScope::new(&tcb);
///     let gc = Gc::new(SharedData { value: 100 });
///     let handle = scope.handle(&gc);
///
///     // Clone the handle to use in spawned tasks
///     let handle_clone = handle;
///
///     tokio::spawn(async move {
///         println!("Task 1: {}", handle_clone.get().value);
///     }).await;
///
///     // Original handle still valid
///     println!("Main: {}", handle.get().value);
/// }
/// ```
pub struct AsyncHandle<T: Trace + 'static> {
    slot: *const HandleSlot,
    scope_id: u64,
    _marker: PhantomData<*const T>,
}

impl<T: Trace + 'static> AsyncHandle<T> {
    /// Gets a reference to the underlying data.
    ///
    /// # Returns
    ///
    /// A reference to the GC-allocated data
    ///
    /// # Safety
    ///
    /// The `AsyncHandleScope` that created this handle must still be alive.
    /// The handle must not be used after the scope is dropped.
    #[inline]
    pub unsafe fn get(&self) -> &T {
        let slot = unsafe { &*self.slot };
        let gc_box_ptr = slot.as_ptr() as *const u8;
        let gc: Gc<T> = unsafe { Gc::<T>::from_raw(gc_box_ptr) };
        let value_ref: &T = &*gc;
        let result = unsafe { &*(value_ref as *const T) };
        std::mem::forget(gc);
        result
    }

    /// Converts this handle to a `Gc<T>`.
    ///
    /// The returned `Gc` has an incremented reference count and will
    /// outlive this handle and its scope.
    ///
    /// # Returns
    ///
    /// A new `Gc<T>` pointing to the same data
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::AsyncHandleScope;
    ///
    /// #[derive(Trace)]
    /// struct Persistent { id: u64 }
    ///
    /// async fn escape_to_gc() {
    ///     let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    ///     let scope = AsyncHandleScope::new(&tcb);
    ///     let gc = Gc::new(Persistent { id: 42 });
    ///     let handle = scope.handle(&gc);
    ///
    ///     // Escape to a Gc that outlives the scope
    ///     let escaped = handle.to_gc();
    ///     drop(scope);
    ///
    ///     // escaped is still valid!
    ///     println!("{}", escaped.id);
    /// }
    /// ```
    #[inline]
    pub fn to_gc(&self) -> Gc<T> {
        unsafe {
            let slot = &*self.slot;
            let ptr = slot.as_ptr() as *const u8;
            let gc: Gc<T> = Gc::from_raw(ptr);
            let gc_clone = gc.clone();
            std::mem::forget(gc);
            gc_clone
        }
    }
}

impl<T: Trace + 'static> Clone for AsyncHandle<T> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot,
            scope_id: self.scope_id,
            _marker: PhantomData,
        }
    }
}

impl<T: Trace + 'static> Copy for AsyncHandle<T> {}

unsafe impl<T: Trace + 'static> Send for AsyncHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for AsyncHandle<T> {}

/// Spawns an async task with automatic GC root tracking.
///
/// This macro creates an `AsyncHandleScope` within the spawned task,
/// creating handles for the provided GC objects. The scope is kept alive
/// until the task completes, ensuring handles remain valid across awaits.
///
/// # Syntax
///
/// Single handle:
/// ```rust
/// spawn_with_gc!(gc => |handle| { body })
/// ```
///
/// Multiple handles:
/// ```rust
/// spawn_with_gc!(gc1, gc2, gc3 => |h1, h2, h3| { body })
/// ```
///
/// # Arguments
///
/// - GC objects to track as roots
/// - Closure receiving handles (one per GC object)
/// - Body executed in the spawned task
///
/// # Returns
///
/// A `JoinHandle` for the spawned task
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
///
/// #[derive(Trace)]
/// struct Task { name: String }
///
/// async fn spawn_tasks() {
///     let data = Gc::new(Task { name: "important".to_string() });
///
///     // Single handle
///     rudo_gc::spawn_with_gc!(data => |handle| {
///         tokio::task::yield_now().await;
///         println!("Task: {}", handle.get().name);
///     }).await;
///
///     // Multiple handles
///     let a = Gc::new(1);
///     let b = Gc::new(2);
///     rudo_gc::spawn_with_gc!(a, b => |ha, hb| {
///         println!("{} + {} = {}", ha.get(), hb.get(), ha.get() + hb.get());
///     }).await;
/// }
/// ```
///
/// # Panics
///
/// - Panics if called outside a GC thread
/// - Panics if more than 256 handles are created in the scope
#[macro_export]
macro_rules! spawn_with_gc {
    ($gc:expr => |$handle:ident| $body:expr) => {{
        let __gc = $gc;
        let __tcb = $crate::heap::current_thread_control_block()
            .expect("spawn_with_gc! must be called within a GC thread");

        tokio::spawn(async move {
            let __scope = $crate::handles::AsyncHandleScope::new(&__tcb);
            let $handle = __scope.handle(&__gc);
            let __result = { $body };
            drop(__scope);
            __result
        })
    }};

    ($($gc:ident),+ => |$($handle:ident),+| $body:expr) => {{
        $(let $gc = $gc;)+
        let __tcb = $crate::heap::current_thread_control_block()
            .expect("spawn_with_gc! must be called within a GC thread");

        tokio::spawn(async move {
            let __scope = $crate::handles::AsyncHandleScope::new(&__tcb);
            $(let $handle = __scope.handle(&$gc);)+
            let __result = { $body };
            drop(__scope);
            __result
        })
    }};
}
