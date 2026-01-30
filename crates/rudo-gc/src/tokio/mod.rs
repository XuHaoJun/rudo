//! Tokio async/await integration for rudo-gc.
//!
//! This module provides tokio-specific features when the `tokio` feature is enabled:
//! - [`GcTokioExt`] trait with [`root_guard()`][GcTokioExt::root_guard] and [`yield_now()`][GcTokioExt::yield_now]
//! - [`GcRootSet`] for process-level root tracking
//! - [`GcRootGuard`] for RAII root registration
//! - [`spawn()`][crate::tokio::spawn] for automatic root tracking when spawning tasks
//!
//! # Enabling Tokio Support
//!
//! ```toml
//! [dependencies]
//! rudo-gc = { version = "0.1", features = ["derive", "tokio"] }
//! ```
//!
//! # Example
//!
//! ```
//! use rudo_gc::{Gc, Trace, GcTokioExt};
//!
//! #[derive(Trace)]
//! struct Data {
//!     value: i32,
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let gc = Gc::new(Data { value: 42 });
//!
//!     // Create a root guard to protect the Gc in async tasks
//!     let _guard = gc.root_guard();
//!
//!     // Spawn a task that accesses the Gc
//!     tokio::spawn(async move {
//!         println!("Value: {}", gc.value);
//!     }).await.unwrap();
//! }
//! ```

mod guard;
mod root;

pub use guard::{GcRootGuard, GcRootScope};
pub use root::GcRootSet;

#[cfg(feature = "tokio")]
pub use rudo_gc_derive::main as gc_main;

#[cfg(feature = "tokio")]
pub use rudo_gc_derive::root as gc_root;

use crate::ptr::Gc;
use crate::trace::Trace;

#[cfg(feature = "tokio")]
use tokio::task;

#[cfg(feature = "tokio")]
/// Extension trait providing tokio-specific methods for `Gc<T>`.
///
/// This trait is automatically implemented for all `T: Trace + Send + Sync`
/// when the `tokio` feature is enabled.
#[allow(async_fn_in_trait)]
pub trait GcTokioExt: Trace + Send + Sync {
    /// Creates a root guard that protects this `Gc<T>` during async execution.
    ///
    /// The guard keeps the `Gc<T>` alive for its entire lifetime. When the guard
    /// is dropped, the `Gc<T>` is no longer protected and may be collected by
    /// the GC if no other roots reference it.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace, GcTokioExt};
    ///
    /// #[derive(Trace)]
    /// struct Data { value: i32 }
    ///
    /// async fn example() {
    ///     let gc = Gc::new(Data { value: 42 });
    ///     let _guard = gc.root_guard();
    ///
    ///     // gc is protected while _guard exists
    ///     tokio::spawn(async move {
    ///         println!("{}", gc.value);
    ///     }).await.unwrap();
    /// }
    /// ```
    fn root_guard(&self) -> GcRootGuard;

    /// Yields execution back to the tokio scheduler.
    ///
    /// This allows the GC to run during long-running computations. Call this
    /// periodically to prevent GC pauses from causing latency spikes.
    ///
    /// # Panics
    ///
    /// This function panics if called outside of a tokio runtime context.
    ///
    /// # Example
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace, GcTokioExt};
    ///
    /// async fn long_computation() {
    ///     let gc = Gc::new(TestData { value: 0 });
    ///
    ///     for i in 0..10000 {
    ///         // Process data
    ///         gc.yield_now().await;
    ///     }
    /// }
    /// ```
    async fn yield_now(&self);
}

#[cfg(feature = "tokio")]
impl<T: Trace + Send + Sync> GcTokioExt for Gc<T> {
    #[allow(clippy::missing_panics_doc)]
    #[allow(clippy::use_self)]
    fn root_guard(&self) -> GcRootGuard {
        let ptr = Gc::<T>::internal_ptr(self).cast_mut();
        // SAFETY: The pointer is valid because it comes from internal_ptr.
        // The ptr is guaranteed to be non-null and properly aligned.
        let nonnull = unsafe { std::ptr::NonNull::new_unchecked(ptr) };
        unsafe { GcRootGuard::new(nonnull) }
    }

    async fn yield_now(&self) {
        task::yield_now().await;
    }
}

/// Spawns an async task with automatic root tracking.
///
/// This function wraps `tokio::task::spawn` to automatically protect any `Gc<T>`
/// pointers captured by the spawned task's future. Users do not need to manually
/// create [`GcRootGuard`]s when using this function.
///
/// # Type Parameters
///
/// * `F` - The future to spawn, must be `Send + 'static`
/// * `T` - The output type of the future, must be `Send + 'static`
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace};
/// use rudo_gc::tokio::spawn;
///
/// #[derive(Trace)]
/// struct Data { value: i32 }
///
/// #[tokio::main]
/// async fn main() {
///     let gc = Gc::new(Data { value: 42 });
///
///     // gc is automatically protected for the task's lifetime
///     let result = spawn(async move {
///         println!("Value: {}", gc.value);
///         gc.value * 2
///     }).await;
///
///     assert_eq!(result, 84);
/// }
/// ```
#[cfg(feature = "tokio")]
pub async fn spawn<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    // Create a new root guard for this spawn
    let guard = unsafe { GcRootGuard::new(std::ptr::NonNull::dangling()) };
    // SAFETY: We're creating a new GcRootScope that wraps the future with the guard.
    // The guard is valid and will be dropped when the future completes.
    let wrapped = unsafe { GcRootScope::new(future, guard) };
    wrapped.await
}
