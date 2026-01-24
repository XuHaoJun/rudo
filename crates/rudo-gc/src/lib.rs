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
//!
//! # Thread Safety
//!
//! `Gc<T>` is `!Send` and `!Sync`. It can only be used within a single thread.
//! For multi-threaded garbage collection, consider future `sync::Gc<T>` support.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::module_name_repetitions)]

pub mod cell;
mod gc;
mod metrics;
mod ptr;
mod scan;
mod stack;
mod trace;
mod trace_closure;

/// `BiBOP` memory management internals.
///
/// This module is public for testing and advanced use cases.
/// Most users should use `Gc<T>` directly.
pub mod heap;

// Re-export public API
pub use cell::GcCell;
pub use gc::{
    collect, collect_full, default_collect_condition, safepoint, set_collect_condition, CollectInfo,
};
pub use metrics::{last_gc_metrics, CollectionType, GcMetrics};
pub use ptr::{Gc, Weak};
pub use scan::scan_heap_region_conservatively;
pub use trace::{Trace, Visitor};
pub use trace_closure::TraceClosure;

// Re-export derive macro when feature is enabled
#[cfg(feature = "derive")]
pub use rudo_gc_derive::Trace;

#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub mod test_util {
    pub use crate::gc::{clear_test_roots, register_test_root};

    /// Get the internal `GcBox` pointer.
    pub fn internal_ptr<T: crate::Trace + ?Sized>(gc: &crate::Gc<T>) -> *const u8 {
        crate::Gc::internal_ptr(gc)
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
}

#[cfg(test)]
mod blacklisting_test;
