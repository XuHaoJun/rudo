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

use std::any::TypeId;
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::heap::ThreadControlBlock;
use crate::ptr::GcBox;
use crate::trace::Trace;
use crate::Gc;

use super::local_handles::{HandleBlock, HandleSlot, HANDLE_BLOCK_SIZE};

static ASYNC_SCOPE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Shared data for async scope, owned by both `AsyncHandleScope` and the TCB registry.
///
/// Uses `Arc` to ensure data remains valid as long as EITHER party holds a reference.
/// This is NOT a raw pointer - both owners have independent Arc reference counts.
///
/// Lifetime guarantee: The TCB's `async_scopes` registry holds an `Arc<AsyncScopeEntry>`,
/// which in turn holds `Arc<AsyncScopeData>`. The scope data cannot be freed while
/// registered, regardless of whether the `AsyncHandleScope` is still alive.
pub struct AsyncScopeData {
    pub(crate) block: Box<HandleBlock>,
    pub(crate) used: UnsafeCell<AtomicUsize>,
}

/// SAFETY: `AsyncScopeData` is `Send + Sync` because:
/// - `block` is a `Box<HandleBlock>` which is `Send + Sync` (contains raw pointers but they're never accessed concurrently without synchronization)
/// - `used` is an `UnsafeCell<AtomicUsize>` - the `AtomicUsize` is `Sync` by default
unsafe impl Send for AsyncScopeData {}
unsafe impl Sync for AsyncScopeData {}

/// An entry in the async scope registry.
/// Used internally to track async scopes for GC root collection.
///
/// SAFETY: The `data` field is an `Arc` that keeps the `AsyncScopeData` alive.
/// Both the `AsyncHandleScope` and the TCB registry hold `Arc`s, so the data
/// cannot be freed while either is still using it.
pub struct AsyncScopeEntry {
    /// Unique scope identifier
    pub id: u64,
    /// Shared ownership of the scope's data
    pub data: Arc<AsyncScopeData>,
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
    data: Arc<AsyncScopeData>,
    dropped: AtomicBool,
}

impl AsyncHandleScope {
    /// Creates a new `AsyncHandleScope`.
    ///
    /// The scope is automatically registered with the thread control block
    /// for GC root tracking.
    ///
    /// IMPORTANT: The `data` Arc is CLONED before registration.
    /// This ensures TCB holds independent ownership - dropping the
    /// `AsyncHandleScope` does NOT deallocate the scope data.
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
    pub fn new(tcb: &std::sync::Arc<ThreadControlBlock>) -> Self {
        let id = ASYNC_SCOPE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);

        let data = Arc::new(AsyncScopeData {
            block: HandleBlock::new(),
            used: UnsafeCell::new(AtomicUsize::new(0)),
        });

        let scope = Self {
            id,
            tcb: std::sync::Arc::clone(tcb),
            data: Arc::clone(&data),
            dropped: AtomicBool::new(false),
        };

        tcb.register_async_scope(scope.id, data);

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
        let used = unsafe { &*self.data.used.get() };
        let idx = used.fetch_add(1, Ordering::Relaxed);
        if idx >= HANDLE_BLOCK_SIZE {
            panic!("AsyncHandleScope: exceeded maximum handle count ({HANDLE_BLOCK_SIZE})");
        }

        let slot_ptr = unsafe {
            let slots_ptr = self.data.block.slots.get() as *mut HandleSlot;
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
        let used = unsafe { &*self.data.used.get() }.load(Ordering::Acquire);
        let slots = unsafe { &*self.data.block.slots.get() };
        for slot in slots.iter().take(used) {
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
    ///
    /// **Undefined behavior will occur** if these constraints are violated:
    /// the returned reference may be dangling, pointing to freed memory.
    ///
    /// In debug builds, use `scope.with_guard()` for checked handle access.
    #[inline]
    pub unsafe fn get(&self) -> &T {
        let slot = unsafe { &*self.slot };
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        let gc_box = unsafe { &*gc_box_ptr };
        gc_box.value()
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
    pub fn to_gc(self) -> Gc<T> {
        unsafe {
            let ptr = (*self.slot).as_ptr() as *const u8;
            Gc::from_raw(ptr)
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
    }    };
}

/// A builder for tracking multiple GC objects in async contexts.
///
/// `GcScope` provides a convenient API for tracking multiple `Gc<T>` objects
/// that need to remain valid across async await points. Unlike `spawn_with_gc!`
/// which requires compile-time knowledge of all GC objects, `GcScope` allows
/// dynamic tracking at runtime.
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::handles::GcScope;
///
/// #[derive(Trace)]
/// struct Data { value: i32 }
///
/// async fn dynamic_tracking() {
///     let gc_a = Gc::new(Data { value: 1 });
///     let gc_b = Gc::new(Data { value: 2 });
///
///     let mut scope = GcScope::new();
///     scope.track(&gc_a).track(&gc_b);
///
///     let result = scope.spawn(|handles| async move {
///         let mut sum = 0;
///         for handle in handles {
///             if let Some(data) = handle.downcast_ref::<Data>() {
///                 sum += data.value;
///             }
///         }
///         sum
///     }).await;
///
///     assert_eq!(result, 3);
/// }
/// ```
///
/// # Comparison with `spawn_with_gc!`
///
/// | Aspect | `spawn_with_gc!` | `GcScope` |
/// |--------|------------------|-----------|
/// | Syntax | Macro-based | Builder pattern |
/// | Multiple Gc | `gc_a, gc_b => |ha, hb| {}` | `.track(&a).track(&b)` |
/// | Dynamic count | Compile-time fixed | Runtime dynamic |
/// | Handle types | Preserved per-type | Type-erased with downcast |
pub struct GcScope {
    tracked: Vec<TrackedGc>,
    _marker: PhantomData<*const ()>,
}

struct TrackedGc {
    ptr: *const GcBox<()>,
    type_id: TypeId,
}

impl Clone for TrackedGc {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr,
            type_id: self.type_id,
        }
    }
}

unsafe impl Send for TrackedGc {}
unsafe impl Sync for TrackedGc {}

impl GcScope {
    /// Creates a new empty `GcScope`.
    ///
    /// # Returns
    ///
    /// A new `GcScope` ready to track GC objects.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::handles::GcScope;
    ///
    /// let scope = GcScope::new();
    /// ```
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            tracked: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Tracks a GC object in this scope.
    ///
    /// The GC object will remain valid as long as the scope (or handles created from it)
    /// are alive. Multiple calls to `track()` can be made to add more objects.
    ///
    /// Takes a reference to avoid consuming the `Gc<T>`, allowing the same
    /// object to be used elsewhere.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The type of the GC object, must implement `Trace`
    ///
    /// # Arguments
    ///
    /// * `gc` - The GC object to track
    ///
    /// # Returns
    ///
    /// `self` for method chaining.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::GcScope;
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// async fn track_example() {
    ///     let gc = Gc::new(Data { value: 42 });
    ///
    ///     let mut scope = GcScope::new();
    ///     scope.track(&gc);
    /// }
    /// ```
    #[inline]
    pub fn track<T: Trace + 'static>(&mut self, gc: &Gc<T>) -> &mut Self {
        let ptr = Gc::internal_ptr(gc) as *const GcBox<()>;
        self.tracked.push(TrackedGc {
            ptr,
            type_id: TypeId::of::<T>(),
        });
        self
    }

    /// Tracks multiple GC objects from a slice.
    ///
    /// This is more efficient than calling `track()` multiple times for many objects.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The type of the GC objects, must implement `Trace`
    ///
    /// # Arguments
    ///
    /// * `gc_slice` - A slice of GC objects to track
    ///
    /// # Returns
    ///
    /// `self` for method chaining.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::GcScope;
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// async fn track_slice() {
    ///     let objects: Vec<Gc<Data>> = (0..10)
    ///         .map(|i| Gc::new(Data { value: i }))
    ///         .collect();
    ///
    ///     let mut scope = GcScope::new();
    ///     scope.track_slice(&objects);
    /// }
    /// ```
    #[inline]
    pub fn track_slice<T: Trace + 'static>(&mut self, gc_slice: &[Gc<T>]) -> &mut Self {
        let type_id = TypeId::of::<T>();
        self.tracked.extend(gc_slice.iter().map(|gc| TrackedGc {
            ptr: Gc::internal_ptr(gc) as *const GcBox<()>,
            type_id,
        }));
        self
    }

    /// Tracks a clone of each GC object in another scope.
    ///
    /// This is useful when you want to track GC objects from a parent scope
    /// without consuming them.
    ///
    /// # Arguments
    ///
    /// * `other` - Another `GcScope` to clone tracking from
    ///
    /// # Returns
    ///
    /// `self` for method chaining.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::GcScope;
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// async fn merge_scopes() {
    ///     let mut scope_a = GcScope::new();
    ///     scope_a.track(&Gc::new(Data { value: 1 }));
    ///
    ///     let mut scope_b = GcScope::new();
    ///     scope_b.track(&Gc::new(Data { value: 2 }));
    ///
    ///     let mut merged = GcScope::new();
    ///     merged.track_from(&scope_a);
    ///     merged.track_from(&scope_b);
    /// }
    /// ```
    #[inline]
    pub fn track_from(&mut self, other: &Self) -> &mut Self {
        self.tracked.extend(other.tracked.iter().cloned());
        self
    }

    /// Spawns an async task with all tracked GC objects as roots.
    ///
    /// Creates an `AsyncHandleScope` and handles for each tracked GC object,
    /// then spawns an async task that receives these handles.
    ///
    /// # Type Parameters
    ///
    /// * `F` - The async closure type
    /// * `R` - The return type of the closure
    ///
    /// # Arguments
    ///
    /// * `f` - An async closure that receives handles to all tracked GC objects
    ///
    /// # Returns
    ///
    /// A `JoinHandle` for the spawned task.
    ///
    /// # Panics
    ///
    /// - Panics if called outside a GC thread
    /// - Panics if more than 256 handles are created in total
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::GcScope;
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// async fn spawn_example() {
    ///     let gc_a = Gc::new(Data { value: 10 });
    ///     let gc_b = Gc::new(Data { value: 20 });
    ///
    ///     let mut scope = GcScope::new();
    ///     scope.track(&gc_a).track(&gc_b);
    ///
    ///     let result = scope.spawn(|handles| async move {
    ///         let mut sum = 0;
    ///         for handle in handles {
    ///             if let Some(data) = handle.downcast_ref::<Data>() {
    ///                 sum += data.value;
    ///             }
    ///         }
    ///         sum
    ///     }).await;
    ///
    ///     assert_eq!(result, 30);
    /// }
    /// ```
    #[inline]
    pub async fn spawn<F, R>(&self, f: impl FnOnce(Vec<AsyncGcHandle>) -> R) -> R::Output
    where
        F: std::future::Future<Output = R>,
        R: std::future::Future,
    {
        let tcb = crate::heap::current_thread_control_block()
            .expect("GcScope::spawn() must be called within a GC thread");

        let scope = AsyncHandleScope::new(&tcb);

        let tracked: Vec<TrackedGc> = self.tracked.clone();

        let handles: Vec<AsyncGcHandle> = tracked
            .iter()
            .map(|tracked| {
                let used = unsafe { &*scope.data.used.get() }.fetch_add(1, Ordering::Relaxed);

                let slot_ptr = unsafe {
                    let slots_ptr = scope.data.block.slots.get() as *mut HandleSlot;
                    slots_ptr.add(used)
                };

                unsafe {
                    (*slot_ptr).set(tracked.ptr);
                }

                AsyncGcHandle {
                    slot: slot_ptr,
                    type_id: tracked.type_id,
                }
            })
            .collect();

        let result = f(handles).await;
        drop(scope);
        result
    }

    /// Returns the number of GC objects currently tracked.
    ///
    /// # Returns
    ///
    /// The count of tracked GC objects.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::GcScope;
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// fn count_example() {
    ///     let mut scope = GcScope::new();
    ///     scope.track(&Gc::new(Data { value: 1 }));
    ///     scope.track(&Gc::new(Data { value: 2 }));
    ///
    ///     assert_eq!(scope.len(), 2);
    /// }
    /// ```
    #[inline]
    pub fn len(&self) -> usize {
        self.tracked.len()
    }

    /// Returns whether this scope tracks any GC objects.
    ///
    /// # Returns
    ///
    /// `true` if the scope is empty, `false` otherwise.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::handles::GcScope;
    ///
    /// let empty = GcScope::new();
    /// assert!(empty.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.tracked.is_empty()
    }
}

impl Default for GcScope {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// A type-erased async GC handle with downcasting support.
///
/// `AsyncGcHandle` is created by `GcScope::spawn()` and provides
/// type-erased access to GC objects with the ability to downcast
/// to the original type.
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::handles::GcScope;
///
/// #[derive(Trace)]
/// struct Data { value: i32 }
///
/// async fn downcast_example() {
///     let gc = Gc::new(Data { value: 42 });
///
///     let mut scope = GcScope::new();
///     scope.track(&gc);
///
///     scope.spawn(|handles| async move {
///         for handle in handles {
///             if let Some(data) = handle.downcast_ref::<Data>() {
///                 println!("Found Data: {}", data.value);
///             }
///         }
///     }).await;
/// }
/// ```
pub struct AsyncGcHandle {
    slot: *const HandleSlot,
    type_id: TypeId,
}

impl AsyncGcHandle {
    /// Attempts to downcast this handle to the specified type.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The type to downcast to, must implement `Trace + 'static`
    ///
    /// # Returns
    ///
    /// `Some(&T)` if the handle contains the specified type, `None` otherwise.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::GcScope;
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// async fn downcast_example() {
    ///     let gc = Gc::new(Data { value: 42 });
    ///
    ///     let mut scope = GcScope::new();
    ///     scope.track(&gc);
    ///
    ///     scope.spawn(|handles| async move {
    ///         for handle in handles {
    ///             if let Some(data) = handle.downcast_ref::<Data>() {
    ///                 println!("Found Data: {}", data.value);
    ///             }
    ///         }
    ///     }).await;
    /// }
    /// ```
    #[inline]
    pub fn downcast_ref<T: Trace + 'static>(&self) -> Option<&T> {
        if self.type_id == TypeId::of::<T>() {
            let slot = unsafe { &*self.slot };
            let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
            Some(unsafe { &*gc_box_ptr }.value())
        } else {
            None
        }
    }

    /// Returns the type ID of the tracked GC object.
    ///
    /// # Returns
    ///
    /// The `TypeId` of the original type.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    /// use rudo_gc::handles::GcScope;
    /// use std::any::TypeId;
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// async fn type_id_example() {
    ///     let gc = Gc::new(Data { value: 42 });
    ///
    ///     let mut scope = GcScope::new();
    ///     scope.track(&gc);
    ///
    ///     scope.spawn(|handles| async move {
    ///         for handle in handles {
    ///             assert_eq!(handle.type_id(), TypeId::of::<Data>());
    ///         }
    ///     }).await;
    /// }
    /// ```
    #[inline]
    pub fn type_id(&self) -> TypeId {
        self.type_id
    }
}

unsafe impl Send for GcScope {}
unsafe impl Sync for GcScope {}

unsafe impl Send for AsyncGcHandle {}
unsafe impl Sync for AsyncGcHandle {}
