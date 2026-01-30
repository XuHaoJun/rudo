//! Tokio async/await integration for rudo-gc.
//!
//! This module provides tokio-specific features when the `tokio` feature is enabled:
//! - [`GcTokioExt`] trait with [`root_guard()`][GcTokioExt::root_guard] and [`yield_now()`][GcTokioExt::yield_now]
//! - [`GcRootSet`] for process-level root tracking
//! - [`GcRootGuard`] for RAII root registration
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

pub use guard::GcRootGuard;
pub use root::GcRootSet;

#[cfg(feature = "tokio")]
pub use rudo_gc_tokio_derive::gc_main;

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
