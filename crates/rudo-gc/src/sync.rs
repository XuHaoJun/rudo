//! Concurrent GC primitives for thread-safe garbage-collected objects.
//!
//! This module provides [`GcRwLock`] and [`GcMutex`] types for safely sharing
//! GC-allocated objects across multiple threads. These types use `parking_lot`
//! synchronization primitives and implement lock-free GC tracing during STW pauses.
//!
//! # Programmers
//!
//! Design inspired by R. Kent Dybvig's work on Scheme implementations and
//! the Rust leadership council's guidance on safe concurrency patterns.
//!
//! # Examples
//!
//! ```
//! use rudo_gc::{Gc, GcRwLock, Trace};
//!
//! #[derive(Trace)]
//! struct SharedData {
//!     value: i32,
//! }
//!
//! let data: Gc<GcRwLock<SharedData>> = Gc::new(GcRwLock::new(SharedData { value: 42 }));
//!
//! // Multiple readers can access concurrently
//! let reader = std::thread::spawn({
//!     let data = Gc::clone(&data);
//!     move || {
//!         let guard = data.read();
//!         guard.value
//!     }
//! });
//!
//! // Writer has exclusive access
//! let mut guard = data.write();
//! guard.value = 100;
//!
//! assert_eq!(reader.join().unwrap(), 42);
//! ```

use parking_lot::{Mutex, RwLock};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

use crate::cell::GcCapture;
use crate::gc::incremental::{
    is_generational_barrier_active, is_incremental_marking_active, FallbackReason,
    IncrementalMarkState,
};
use crate::ptr::GcBox;
use crate::Trace;

mod private {}

/// Marks GC pointers in the value as black when barrier is active.
/// Ensures consistency with `GcCell::borrow_mut()` — new pointers are marked
/// immediately on acquisition, not deferred to guard drop. (bug198)
#[inline]
fn mark_gc_ptrs_immediate<T: GcCapture + ?Sized>(value: &T, barrier_active: bool) {
    if !barrier_active {
        return;
    }
    unsafe {
        let mut gc_ptrs = Vec::with_capacity(32);
        value.capture_gc_ptrs_into(&mut gc_ptrs);
        if !gc_ptrs.is_empty() {
            crate::heap::with_heap(|_heap| {
                for gc_ptr in gc_ptrs {
                    let _ = crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8);
                }
            });
        }
    }
}

/// Records old GC pointer values to SATB buffer with pre-cached barrier state.
/// This avoids TOCTOU by using the cached state instead of re-checking.
#[inline]
fn record_satb_old_values_with_state<T: GcCapture + ?Sized>(value: &T, incremental_active: bool) {
    if !incremental_active {
        return;
    }
    let mut gc_ptrs = Vec::with_capacity(32);
    value.capture_gc_ptrs_into(&mut gc_ptrs);
    if gc_ptrs.is_empty() {
        return;
    }
    if crate::heap::try_with_heap(|heap| {
        for gc_ptr in &gc_ptrs {
            if !heap.record_satb_old_value(*gc_ptr) {
                IncrementalMarkState::global().request_fallback(FallbackReason::SatbBufferOverflow);
                break;
            }
        }
        true
    })
    .is_none()
    {
        for gc_ptr in gc_ptrs {
            // FIX bug330: Check return value and request fallback if buffer overflowed.
            // Note: push_cross_thread_satb also requests fallback internally, but
            // checking here ensures we don't silently drop pointers.
            if !crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr) {
                crate::gc::incremental::IncrementalMarkState::global()
                    .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
            }
        }
    }
}

/// Reader-writer lock wrapper for GC objects.
///
/// `GcRwLock<T>` provides shared read access and exclusive write access to a GC-allocated
/// object. Multiple readers can access the data concurrently, while writers have exclusive
/// access. This is optimal for read-heavy workloads.
///
/// During GC STW pauses, the lock is bypassed to prevent deadlocks - the GC traces
/// the inner data directly without acquiring the lock.
///
/// # Traits
///
/// - `Send + Sync`: When `T: Trace + Send + Sync`
/// - `Trace`: Traces inner data without acquiring lock
///
/// # Examples
///
/// ```
/// use rudo_gc::{Gc, GcRwLock, Trace};
///
/// #[derive(Trace)]
/// struct Cache {
///     entries: usize,
/// }
///
/// let cache: Gc<GcRwLock<Cache>> = Gc::new(GcRwLock::new(Cache { entries: 0 }));
///
/// // Multiple readers
/// let handles: Vec<_> = (0..4).map(|_| {
///     std::thread::spawn({
///         let cache = Gc::clone(&cache);
///         move || {
///             let guard = cache.read();
///             guard.entries
///         }
///     })
/// }).collect();
///
/// // Exclusive writer
/// {
///     let mut guard = cache.write();
///     guard.entries = 100;
/// }
///
/// for handle in handles {
///     assert_eq!(handle.join().unwrap(), 0);
/// }
/// ```
pub struct GcRwLock<T: ?Sized> {
    inner: RwLock<T>,
}

impl<T: ?Sized> GcRwLock<T> {
    #[inline]
    fn trigger_write_barrier_with_state(
        &self,
        generational_active: bool,
        incremental_active: bool,
    ) {
        if generational_active || incremental_active {
            let ptr = std::ptr::from_ref(self).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, incremental_active);
        }
    }

    /// Creates a new `GcRwLock` wrapping the given value.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcRwLock, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data(i32);
    ///
    /// let lock = GcRwLock::new(Data(42));
    /// ```
    #[inline]
    #[must_use]
    pub const fn new(value: T) -> Self
    where
        T: Sized,
    {
        Self {
            inner: RwLock::new(value),
        }
    }

    /// Acquires a read lock, returning a guard that dereferences to the inner data.
    ///
    /// Multiple readers can hold the lock simultaneously. Returns immediately if
    /// the lock is not held by a writer.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcRwLock, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data { value: 10 }));
    ///
    /// let guard = data.read();
    /// assert_eq!(guard.value, 10);
    /// ```
    #[inline]
    pub fn read(&self) -> GcRwLockReadGuard<'_, T>
    where
        T: GcCapture,
    {
        let guard = self.inner.read();
        GcRwLockReadGuard {
            guard,
            _marker: PhantomData,
        }
    }

    /// Attempts to acquire a read lock.
    ///
    /// Returns `Some` with a read guard if the lock is not held by a writer,
    /// or `None` if a writer currently holds the lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcRwLock, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data { value: 10 }));
    ///
    /// if let Some(guard) = data.try_read() {
    ///     assert_eq!(guard.value, 10);
    /// }
    /// ```
    #[inline]
    pub fn try_read(&self) -> Option<GcRwLockReadGuard<'_, T>>
    where
        T: GcCapture,
    {
        self.inner.try_read().map(|guard| GcRwLockReadGuard {
            guard,
            _marker: PhantomData,
        })
    }

    /// Acquires a write lock, returning a guard that dereferences mutably to the inner data.
    ///
    /// Writers have exclusive access. Readers and other writers are blocked until
    /// all guards are dropped.
    ///
    /// Triggers generational and SATB write barriers on acquisition.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcRwLock, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data { value: 10 }));
    ///
    /// {
    ///     let mut guard = data.write();
    ///     guard.value = 20;
    /// }
    ///
    /// assert_eq!(data.read().value, 20);
    /// ```
    #[inline]
    pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
    where
        T: GcCapture,
    {
        let guard = self.inner.write();
        // Cache barrier state AFTER acquiring lock to avoid stale values if incremental
        // marking started while blocked (matches GcThreadSafeCell::borrow_mut).
        let incremental_active = is_incremental_marking_active();
        let generational_active = is_generational_barrier_active();
        // FIX bug432: Always record SATB OLD values at write() time, not just when
        // incremental_active is true. This ensures OLD values are preserved if
        // incremental marking starts after lock acquisition but before drop().
        record_satb_old_values_with_state(&*guard, true);
        self.trigger_write_barrier_with_state(generational_active, incremental_active);
        // FIX bug479: Always mark GC pointers black when OLD values were recorded.
        // If incremental becomes active between entry and here, we must mark NEW
        // to maintain SATB consistency (OLD recorded, NEW must be marked too).
        mark_gc_ptrs_immediate(&*guard, true);
        GcRwLockWriteGuard {
            guard,
            _marker: PhantomData,
        }
    }

    /// Attempts to acquire a write lock.
    ///
    /// Returns `Some` with a write guard if no readers or writers hold the lock,
    /// or `None` if the lock is currently held.
    ///
    /// **Note**: The write barrier is only triggered when the lock is successfully
    /// acquired (returns `Some`). This is correct because there is no old value
    /// to record for SATB if no write occurs.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcRwLock, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data { value: 10 }));
    ///
    /// if let Some(mut guard) = data.try_write() {
    ///     guard.value = 20;
    /// }
    /// ```
    #[inline]
    pub fn try_write(&self) -> Option<GcRwLockWriteGuard<'_, T>>
    where
        T: GcCapture,
    {
        self.inner.try_write().map(|guard| {
            let incremental_active = is_incremental_marking_active();
            let generational_active = is_generational_barrier_active();
            // FIX bug432: Always record SATB OLD values at write() time.
            record_satb_old_values_with_state(&*guard, true);
            self.trigger_write_barrier_with_state(generational_active, incremental_active);
            // FIX bug479: Always mark GC pointers black when OLD values were recorded.
            // If incremental becomes active between entry and here, we must mark NEW
            // to maintain SATB consistency (OLD recorded, NEW must be marked too).
            mark_gc_ptrs_immediate(&*guard, true);
            GcRwLockWriteGuard {
                guard,
                _marker: PhantomData,
            }
        })
    }

    /// Returns `true` if a writer currently holds the lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcRwLock, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data { value: 10 }));
    ///
    /// assert!(!data.is_locked());
    ///
    /// let _guard = data.write();
    /// assert!(data.is_locked());
    /// ```
    #[inline]
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }
}

impl<T> std::fmt::Debug for GcRwLock<T>
where
    T: std::fmt::Debug + GcCapture + ?Sized,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.read(), f)
    }
}

impl<T> Default for GcRwLock<T>
where
    T: Default + Sized,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Clone for GcRwLock<T>
where
    T: Clone + GcCapture + Sized,
{
    fn clone(&self) -> Self {
        Self::new(self.read().clone())
    }
}

/// Read guard for [`GcRwLock`].
///
/// Holds a read lock on the `GcRwLock` and provides access to the inner data.
/// The lock is released when the guard is dropped. Barriers are triggered on drop
/// (when incremental marking is active) to capture GC pointer changes.
///
/// Requires `T: GcCapture`; use [`impl_gc_capture`](crate::impl_gc_capture) for types without GC pointers.
///
/// Access via [`Deref`] yields `&T`.
pub struct GcRwLockReadGuard<'a, T: GcCapture + ?Sized> {
    guard: parking_lot::RwLockReadGuard<'a, T>,
    _marker: PhantomData<&'a T>,
}

impl<T: GcCapture + ?Sized> Deref for GcRwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.deref()
    }
}

impl<T: GcCapture + ?Sized> Drop for GcRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        // Read guard does not modify data — no barrier needed.
        // SATB records old values before overwrite; read does not overwrite.
        // Generational barrier records OLD→YOUNG writes; read does not write.
        // The parking_lot guard releases the read lock automatically.
    }
}

/// Write guard for [`GcRwLock`].
///
/// Holds a write lock on the `GcRwLock` and provides exclusive access to the inner data.
/// The lock is released when the guard is dropped. Barriers are triggered on guard acquisition
/// and again on drop (when incremental marking is active) to capture GC pointer changes.
///
/// Requires `T: GcCapture`; use [`impl_gc_capture`](crate::impl_gc_capture) for types without GC pointers.
///
/// Access via [`Deref`] yields `&T`, [`DerefMut`] yields `&mut T`.
pub struct GcRwLockWriteGuard<'a, T: GcCapture + ?Sized> {
    guard: parking_lot::RwLockWriteGuard<'a, T>,
    _marker: PhantomData<&'a T>,
}

impl<T: GcCapture + ?Sized> Deref for GcRwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.deref()
    }
}

impl<T: GcCapture + ?Sized> DerefMut for GcRwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.deref_mut()
    }
}

/// Drop implementation for write guards.
/// Captures and marks GC pointers on drop to satisfy SATB when incremental marking is active,
/// ensuring modifications made while holding the lock are visible to the GC.
///
/// # TOCTOU between capture and mark
///
/// There is a window between `capture_gc_ptrs_into` and `mark_object_black` where lazy sweep
/// may reclaim and reuse a slot. `mark_object_black` handles this internally: it checks
/// `is_allocated` before marking and uses post-CAS validation to roll back if the slot was
/// swept between check and mark. No explicit pre-check is needed here.
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        // FIX bug409: Re-check current barrier state instead of using cached values.
        // The incremental marking phase may have started after lock acquisition.
        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();

        // Mark new GC pointers black only during incremental marking (bug302).
        // Generational barrier should only mark page as dirty, not prevent collection.
        if incremental_active {
            for gc_ptr in &ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }

        // Call barrier when either generational or incremental marking is active (bug122).
        // Incremental marking needs remembered buffer update; generational needs dirty page.
        if generational_active || incremental_active {
            let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, incremental_active);
        }
    }
}

/// Exclusive mutex wrapper for GC objects.
///
/// `GcMutex<T>` provides exclusive access to a GC-allocated object.
/// Only one thread can access the data at a time. This is optimal for
/// write-heavy workloads or when simple exclusive access is sufficient.
///
/// During GC STW pauses, the lock is bypassed to prevent deadlocks.
///
/// # Traits
///
/// - `Send + Sync`: When `T: Trace + Send + Sync`
/// - `Trace`: Traces inner data without acquiring lock
///
/// # Examples
///
/// ```
/// use rudo_gc::{Gc, GcMutex, Trace};
///
/// #[derive(Trace)]
/// struct Counter {
///     count: i32,
/// }
///
/// let counter: Gc<GcMutex<Counter>> = Gc::new(GcMutex::new(Counter { count: 0 }));
///
/// for _ in 0..10 {
///     let mut guard = counter.lock();
///     guard.count += 1;
/// }
///
/// assert_eq!(counter.lock().count, 10);
/// ```
pub struct GcMutex<T: ?Sized> {
    inner: Mutex<T>,
}

impl<T: ?Sized> GcMutex<T> {
    #[inline]
    fn trigger_write_barrier_with_state(
        &self,
        generational_active: bool,
        incremental_active: bool,
    ) {
        if generational_active || incremental_active {
            let ptr = std::ptr::from_ref(self).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, incremental_active);
        }
    }

    /// Creates a new `GcMutex` wrapping the given value.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcMutex, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data(i32);
    ///
    /// let lock = GcMutex::new(Data(42));
    /// ```
    #[inline]
    #[must_use]
    pub const fn new(value: T) -> Self
    where
        T: Sized,
    {
        Self {
            inner: Mutex::new(value),
        }
    }

    /// Acquires the mutex, returning a guard that dereferences mutably to the inner data.
    ///
    /// The lock is released when the guard is dropped.
    ///
    /// Triggers generational and SATB write barriers on acquisition.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcMutex, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcMutex<Data>> = Gc::new(GcMutex::new(Data { value: 10 }));
    ///
    /// {
    ///     let mut guard = data.lock();
    ///     guard.value = 20;
    /// }
    ///
    /// assert_eq!(data.lock().value, 20);
    /// ```
    #[inline]
    pub fn lock(&self) -> GcMutexGuard<'_, T>
    where
        T: GcCapture,
    {
        let guard = self.inner.lock();
        // Cache barrier state AFTER acquiring lock to avoid stale values if incremental
        // marking started while blocked (matches GcThreadSafeCell::borrow_mut).
        let incremental_active = is_incremental_marking_active();
        let generational_active = is_generational_barrier_active();
        // FIX bug432: Always record SATB OLD values at lock() time.
        record_satb_old_values_with_state(&*guard, true);
        self.trigger_write_barrier_with_state(generational_active, incremental_active);
        // FIX bug480: Always mark GC pointers black when OLD values were recorded.
        // If incremental becomes active between entry and here, we must mark NEW
        // to maintain SATB consistency (OLD recorded, NEW must be marked too).
        mark_gc_ptrs_immediate(&*guard, true);
        GcMutexGuard {
            guard,
            _marker: PhantomData,
        }
    }

    /// Attempts to acquire the mutex.
    ///
    /// Returns `Some` with a mutex guard if the lock is not held,
    /// or `None` if the lock is currently held by another thread.
    ///
    /// Triggers generational and SATB write barriers on acquisition, same as [`lock()`](Self::lock).
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcMutex, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcMutex<Data>> = Gc::new(GcMutex::new(Data { value: 10 }));
    ///
    /// if let Some(mut guard) = data.try_lock() {
    ///     guard.value = 20;
    /// }
    /// ```
    #[inline]
    pub fn try_lock(&self) -> Option<GcMutexGuard<'_, T>>
    where
        T: GcCapture,
    {
        self.inner.try_lock().map(|guard| {
            let incremental_active = is_incremental_marking_active();
            let generational_active = is_generational_barrier_active();
            // FIX bug432: Always record SATB OLD values at lock() time.
            record_satb_old_values_with_state(&*guard, true);
            self.trigger_write_barrier_with_state(generational_active, incremental_active);
            // FIX bug480: Always mark GC pointers black when OLD values were recorded.
            // If incremental becomes active between entry and here, we must mark NEW
            // to maintain SATB consistency (OLD recorded, NEW must be marked too).
            mark_gc_ptrs_immediate(&*guard, true);
            GcMutexGuard {
                guard,
                _marker: PhantomData,
            }
        })
    }

    /// Returns `true` if the mutex is currently locked.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, GcMutex, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// let data: Gc<GcMutex<Data>> = Gc::new(GcMutex::new(Data { value: 10 }));
    ///
    /// assert!(!data.is_locked());
    ///
    /// let _guard = data.lock();
    /// assert!(data.is_locked());
    /// ```
    #[inline]
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }
}

impl<T> std::fmt::Debug for GcMutex<T>
where
    T: std::fmt::Debug + GcCapture + ?Sized,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.lock(), f)
    }
}

impl<T> Default for GcMutex<T>
where
    T: Default + Sized,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Clone for GcMutex<T>
where
    T: Clone + GcCapture + Sized,
{
    fn clone(&self) -> Self {
        Self::new((*self.lock()).clone())
    }
}

/// Guard for [`GcMutex`].
///
/// Holds the mutex lock and provides exclusive access to the inner data.
/// The lock is released when the guard is dropped. Barriers are triggered on guard acquisition
/// and again on drop (when incremental marking is active) to capture GC pointer changes.
///
/// Requires `T: GcCapture`; use [`impl_gc_capture`](crate::impl_gc_capture) for types without GC pointers.
///
/// Access via [`Deref`] yields `&T`, [`DerefMut`] yields `&mut T`.
pub struct GcMutexGuard<'a, T: GcCapture + ?Sized> {
    guard: parking_lot::MutexGuard<'a, T>,
    _marker: PhantomData<&'a T>,
}

impl<T: GcCapture + ?Sized> Deref for GcMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.deref()
    }
}

impl<T: GcCapture + ?Sized> DerefMut for GcMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.deref_mut()
    }
}

/// Drop implementation for mutex guards.
/// Captures and marks GC pointers on drop to satisfy SATB when incremental marking is active,
/// ensuring modifications made while holding the lock are visible to the GC.
///
/// # TOCTOU between capture and mark
///
/// There is a window between `capture_gc_ptrs_into` and `mark_object_black` where lazy sweep
/// may reclaim and reuse a slot. `mark_object_black` handles this internally: it checks
/// `is_allocated` before marking and uses post-CAS validation to roll back if the slot was
/// swept between check and mark. No explicit pre-check is needed here.
impl<T: GcCapture + ?Sized> Drop for GcMutexGuard<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        // FIX bug409: Re-check current barrier state instead of using cached values.
        // The incremental marking phase may have started after lock acquisition.
        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();

        // FIX bug304: Only mark GC pointers black during incremental marking, not generational barrier.
        if incremental_active {
            for gc_ptr in &ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }

        // Call barrier when either generational or incremental marking is active (bug122).
        if generational_active || incremental_active {
            let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, incremental_active);
        }
    }
}

unsafe impl<T: Trace + ?Sized> Trace for GcRwLock<T> {
    fn trace(&self, visitor: &mut impl crate::Visitor) {
        // SAFETY:
        // 1. GC runs in STW pause - all mutator threads are suspended
        // 2. data_ptr() returns a raw pointer to the inner data
        // 3. No other thread can modify the data during STW - safe to read without lock
        // 4. Atomic pointer writes ensure consistent reads even if thread suspended mid-write
        // This is the same pattern used in production GC implementations (Go, Java, .NET).
        let raw_ptr = self.inner.data_ptr();
        // SAFETY: See above safety proof.
        unsafe { (*raw_ptr).trace(visitor) }
    }
}

impl<T: GcCapture + ?Sized> GcCapture for GcRwLock<T> {
    /// Returns empty slice because inner data requires locking.
    ///
    /// Lock-protected types cannot return a static slice; pointer collection
    /// must use [`capture_gc_ptrs_into()`](GcCapture::capture_gc_ptrs_into) which
    /// acquires the lock and delegates to the inner value.
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Use blocking read() to reliably capture all GC pointers. try_read() would
        // silently miss pointers when a writer holds the lock, breaking SATB.
        let guard = self.inner.read();
        guard.capture_gc_ptrs_into(ptrs);
    }
}

unsafe impl<T: Trace + ?Sized> Trace for GcMutex<T> {
    fn trace(&self, visitor: &mut impl crate::Visitor) {
        // SAFETY: Same rationale as GcRwLock.
        // During STW pause, all mutators are suspended, making lock bypass safe.
        let raw_ptr = self.inner.data_ptr();
        // SAFETY: See safety proof for GcRwLock.
        unsafe { (*raw_ptr).trace(visitor) }
    }
}

impl<T: GcCapture + ?Sized> GcCapture for GcMutex<T> {
    /// Returns empty slice because inner data requires locking.
    ///
    /// See [`GcRwLock`]'s `capture_gc_ptrs()` for rationale.
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Use blocking lock() to reliably capture all GC pointers, consistent with
        // GcRwLock::capture_gc_ptrs_into(). try_lock() would silently miss pointers
        // when a writer holds the lock, potentially breaking SATB.
        let guard = self.inner.lock();
        guard.capture_gc_ptrs_into(ptrs);
    }
}

unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcRwLock<T> {}

unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcRwLock<T> {}

unsafe impl<T: Trace + Send + Sync + ?Sized> Send for GcMutex<T> {}

unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for GcMutex<T> {}
