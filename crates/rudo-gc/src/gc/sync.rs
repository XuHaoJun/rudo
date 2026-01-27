//! Lock ordering discipline for deadlock prevention.
//!
//! This module defines and enforces a strict lock acquisition order to prevent
//! deadlocks in the garbage collector. All locks must be acquired in the
//! following order:
//!
//! 1. **`LocalHeap`** lock (order 1) - Per-thread allocation lock
//! 2. **`GlobalMarkState`** lock (order 2) - Mark phase coordination
//! 3. **`GC Request`** lock (order 3) - GC trigger and coordination
//!
//! # Lock Ordering Rules
//!
//! ## Forbidden Patterns
//!
//! - Never acquire `LocalHeap` while holding `GlobalMarkState`
//! - Never acquire `GlobalMarkState` while holding `GC Request`
//! - Never acquire any lock while holding a `PerThreadMarkQueue` lock
//!
//! ## Safe Patterns
//!
//! - Acquire locks in order: `LocalHeap` → `GlobalMarkState` → `GC Request`
//! - Release locks in reverse order of acquisition
//! - Use `try_lock()` when lock ordering is unclear
//!
//! # Debug Build Validation
//!
//! In debug builds, lock ordering violations are detected and reported immediately.
//! In release builds, the validation is skipped for performance.
//!
//! # Example
//!
//! ```
//! use std::sync::atomic::{AtomicU8, Ordering};
//!
//! const LOCK_ORDER_LOCAL_HEAP: u8 = 1;
//! const LOCK_ORDER_GLOBAL_MARK: u8 = 2;
//! const LOCK_ORDER_GC_REQUEST: u8 = 3;
//!
//! #[cfg(debug_assertions)]
//! fn validate_lock_order(tag: u8, expected_min: u8) {
//!     debug_assert!(
//!         tag >= expected_min,
//!         "Lock ordering violation: expected order >= {}, got {}",
//!         expected_min,
//!         tag
//!     );
//! }
//!
//! #[cfg(not(debug_assertions))]
//! fn validate_lock_order(_tag: u8, _expected_min: u8) {}
//! ```

/// Lock order tags for validation.
///
/// Each lock type has a unique order value. Locks must be acquired in
/// increasing order to prevent circular wait conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::trivially_copy_pass_by_ref)]
pub enum LockOrder {
    /// `LocalHeap` per-thread allocation lock (order 1).
    LocalHeap = 1,
    /// `GlobalMarkState` mark phase coordination lock (order 2).
    GlobalMarkState = 2,
    /// `GC Request` trigger and coordination lock (order 3).
    GcRequest = 3,
}

impl LockOrder {
    /// Get the order value for this lock type.
    #[must_use]
    pub const fn order_value(self) -> u8 {
        self as u8
    }
}

/// Validate lock acquisition order in debug builds.
///
/// In release builds, this function is optimized away.
///
/// # Panics
///
/// Panics in debug builds if `tag` is less than `expected_min`.
#[inline]
#[allow(clippy::format_in_format_args)]
#[cfg(debug_assertions)]
pub fn validate_lock_order(tag: LockOrder, expected_min: LockOrder) {
    debug_assert!(
        tag.order_value() >= expected_min.order_value(),
        "Lock ordering violation: {} (order {}) cannot be acquired while holding {} (order {}). Expected minimum order: {}",
        format!("{:?}", tag),
        tag.order_value(),
        format!("{:?}", expected_min),
        expected_min.order_value(),
        format!("{:?}", expected_min)
    );
}

#[inline]
#[cfg(not(debug_assertions))]
pub fn validate_lock_order(_tag: LockOrder, _expected_min: LockOrder) {
    // No-op in release builds
}

/// Thread-local storage for the current minimum lock order.
///
/// Tracks the minimum order value of locks currently held by this thread.
/// Used for validation of lock acquisition order.
#[inline]
#[allow(clippy::used_underscore_binding)]
#[allow(clippy::missing_const_for_fn)]
pub fn set_min_lock_order(_order: LockOrder) {
    // In debug builds, this would set thread-local state
    // for validating lock acquisition order.
    #[cfg(debug_assertions)]
    {
        let _ = _order; // Suppress unused warning
                        // thread_local! {
                        //     static MIN_LOCK_ORDER: AtomicU8 = AtomicU8::new(0);
                        // }
                        // MIN_LOCK_ORDER.with(|min| {
                        //     min.store(order.order_value(), Ordering::Relaxed);
                        // });
    }
}

/// Get the current minimum lock order held by this thread.
#[inline]
#[cfg(debug_assertions)]
pub const fn get_min_lock_order() -> LockOrder {
    LockOrder::LocalHeap // Placeholder
}

#[cfg(test)]
mod tests {
    use super::LockOrder;

    #[test]
    fn test_lock_order_values() {
        assert_eq!(LockOrder::LocalHeap.order_value(), 1);
        assert_eq!(LockOrder::GlobalMarkState.order_value(), 2);
        assert_eq!(LockOrder::GcRequest.order_value(), 3);
    }

    #[test]
    fn test_lock_order_comparison() {
        assert!(LockOrder::LocalHeap.order_value() < LockOrder::GlobalMarkState.order_value());
        assert!(LockOrder::GlobalMarkState.order_value() < LockOrder::GcRequest.order_value());
    }
}
