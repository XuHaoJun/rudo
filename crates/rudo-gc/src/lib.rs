//! A garbage-collected smart pointer library for Rust.
//!
//! `rudo-gc` provides a `Gc<T>` smart pointer with automatic memory reclamation
//! and cycle detection. It uses a **`BiBOP` (Big Bag of Pages)** memory layout for
//! efficient O(1) allocation and a **Mark-Sweep** garbage collection algorithm.
//!
//! # Features
//!
//! - **Automatic cycle detection**: Unlike `Rc<T>`, `Gc<T>` can collect cyclic references
//! - **`BiBOP` memory layout**: O(1) allocation with size-class based segments
//! - **Non-moving GC**: Address stability for Rust's `&T` references
//! - **Ergonomic API**: Similar to `Rc<T>` with `#[derive(Trace)]` for custom types
//!
//! # Quick Start
//!
//! ```ignore
//! use rudo_gc::{Gc, Trace};
//!
//! // Simple allocation
//! let x = Gc::new(42);
//! println!("Value: {}", *x);
//!
//! // Custom types with derive
//! #[derive(Trace)]
//! struct Node {
//!     value: i32,
//!     next: Option<Gc<Node>>,
//! }
//!
//! let node = Gc::new(Node { value: 1, next: None });
//! ```
//!
//! # Handling Cycles
//!
//! ```ignore
//! use rudo_gc::{Gc, Trace, collect};
//! use std::cell::RefCell;
//!
//! #[derive(Trace)]
//! struct Node {
//!     next: RefCell<Option<Gc<Node>>>,
//! }
//!
//! let a = Gc::new(Node { next: RefCell::new(None) });
//! let b = Gc::new(Node { next: RefCell::new(None) });
//!
//! // Create cycle: a -> b -> a
//! *a.next.borrow_mut() = Some(Gc::clone(&b));
//! *b.next.borrow_mut() = Some(Gc::clone(&a));
//!
//! drop(a);
//! drop(b);
//! collect(); // Cycle is detected and freed
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::clone_on_copy)]

pub mod cell;
pub mod gc;
pub mod handles;
mod metrics;
mod ptr;
mod scan;
mod stack;
mod trace;
mod trace_closure;

#[cfg(feature = "tokio")]
pub mod tokio;

/// `BiBOP` memory management internals.
///
/// This module is public for testing and advanced use cases.
/// Most users should use `Gc<T>` directly.
pub mod heap;

// Re-export public API
pub use cell::GcCell;
pub use gc::incremental::{
    is_incremental_marking_active, is_write_barrier_active, mark_new_object_black,
    IncrementalConfig, IncrementalMarkState, MarkPhase, MarkSliceResult, MarkStats,
};

/// Configure incremental marking settings.
///
/// Use this to enable and configure incremental marking for reduced GC pause times.
pub fn set_incremental_config(config: gc::incremental::IncrementalConfig) {
    IncrementalMarkState::global().set_config(config);
}

/// Check if incremental GC is enabled.
#[must_use]
pub fn is_incremental_gc_enabled() -> bool {
    IncrementalMarkState::global().config().enabled
}

/// Get the current incremental marking configuration.
#[must_use]
pub fn get_incremental_config() -> gc::incremental::IncrementalConfig {
    *IncrementalMarkState::global().config()
}

/// Yield to the garbage collector for cooperative scheduling.
///
/// This function allows the GC to run during long-running computations,
/// which is particularly useful when incremental marking is enabled.
/// When incremental marking is not active, this is a no-op.
///
/// # Examples
///
/// ```ignore
/// use rudo_gc::Gc;
///
/// fn process_large_dataset(items: &[Item]) {
///     for (i, item) in items.iter().enumerate() {
///         process_item(item);
///
///         // Yield every 1000 items to allow GC marking
///         if i % 1000 == 0 {
///             Gc::<()>::yield_now();
///         }
///     }
/// }
/// ```
pub fn yield_now() {
    if crate::gc::incremental::is_incremental_marking_active() {
        let config = get_incremental_config();
        let budget = config.increment_size;
        crate::heap::with_heap(|heap| {
            let _ = crate::gc::incremental::incremental_mark_slice(heap, budget);
        });
    }
}
pub use gc::{
    collect, collect_full, default_collect_condition, safepoint, set_collect_condition,
    CollectInfo, PerThreadMarkQueue, StealQueue,
};
pub use handles::{
    AsyncHandle, AsyncHandleGuard, AsyncHandleScope, EscapeableHandleScope, Handle, HandleScope,
    MaybeHandle, SealedHandleScope,
};
pub use metrics::{last_gc_metrics, CollectionType, GcMetrics};
pub use ptr::{Gc, GcBox, Weak};
pub use scan::scan_heap_region_conservatively;
pub use trace::{Trace, Visitor};
pub use trace_closure::TraceClosure;

// Re-export derive macros when feature is enabled
#[cfg(feature = "derive")]
pub use rudo_gc_derive::Trace;

#[doc(hidden)]
pub mod test_util {
    pub use crate::gc::{clear_test_roots, register_test_root};

    #[cfg(any(test, feature = "test-util"))]
    pub use crate::gc::iter_test_roots;

    /// Get the internal `GcBox` pointer.
    pub fn internal_ptr<T: crate::Trace>(gc: &crate::Gc<T>) -> *const u8 {
        crate::Gc::internal_ptr(gc)
    }

    /// Reconstruct a `Gc` from an internal pointer.
    ///
    /// # Safety
    ///
    /// The pointer must be a valid, currently allocated `GcBox<T>`.
    #[must_use]
    pub unsafe fn from_raw<T: crate::Trace + 'static>(ptr: *const u8) -> crate::Gc<T> {
        unsafe { crate::Gc::from_raw(ptr) }
    }

    /// Clear CPU registers to prevent stale pointer values from being treated as roots.
    ///
    /// This is useful in tests to ensure objects are collected even when
    /// stale pointer values remain in callee-saved registers after function returns.
    ///
    /// # Safety
    ///
    /// This function clears callee-saved registers (R12-R15 on `x86_64`).
    /// It should only be called when those registers don't contain values
    /// needed by the calling code.
    pub unsafe fn clear_registers() {
        // SAFETY: Caller guarantees that callee-saved registers don't contain
        // values needed by the calling code.
        unsafe { crate::stack::clear_registers() };
    }

    /// Reset all global GC state for test isolation.
    ///
    /// This function clears:
    /// - Thread registry (unregisters all threads)
    /// - Segment manager (frees all pages)
    /// - GC requested flag
    /// - Page size cache
    /// - Test roots
    ///
    /// Call this at the start of each test to ensure clean state:
    ///
    /// ```ignore
    /// use rudo_gc::test_util::reset;
    ///
    /// #[test]
    /// fn my_test() {
    ///     reset();
    ///     // Test code with clean GC state
    /// }
    /// ```
    pub fn reset() {
        unsafe { crate::heap::reset_for_testing() };
        clear_test_roots();
        crate::gc::incremental::IncrementalMarkState::global().reset();
    }
}

#[cfg(test)]
mod blacklisting_test;
