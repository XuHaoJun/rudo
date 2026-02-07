//! # Lock Ordering Discipline for Deadlock Prevention
//!
//! This module defines and enforces a strict lock acquisition order to prevent
//! deadlocks in the garbage collector.
//!
//! ## Global Lock Order
//!
//! All locks must be acquired in the following order:
//!
//! | Level | Lock Type       | Order Value | Description                      |
//! |-------|----------------|-------------|----------------------------------|
//! | 1     | `LocalHeap`      | 1           | Per-thread allocation             |
//! | 1     | `SegmentManager` | 2           | Global memory management         |
//! | 2     | `GlobalMarkState`| 3           | Mark phase coordination          |
//! | 3     | `GcRequest`      | 4           | GC trigger and coordination       |
//!
//! ## Lock Ordering Rules
//!
//! ### Acquisition Rules
//!
//! 1. **Increasing Order**: Locks must be acquired in increasing level (1 → 2 → 3)
//! 2. **Same Level**: Locks with level 1 (`LocalHeap`, `SegmentManager`) can be
//!    acquired in any order relative to each other
//! 3. **Reverse Release**: Release locks in reverse order of acquisition
//!
//! ### Forbidden Patterns
//!
//! - Never acquire `LocalHeap` while holding `GlobalMarkState`
//! - Never acquire `GlobalMarkState` while holding `GcRequest`
//! - Never acquire any lock while holding a `PerThreadMarkQueue` reference
//!
//! ## Validation
//!
//! In debug builds, lock ordering is validated automatically:
//!
//! - `acquire_lock(tag, current_level)`: Called before acquiring a lock
//! - `LockGuard::new(tag)`: RAII-style guard that validates on creation
//!
//! # Examples
//!
//! ## Safe Lock Acquisition
//!
//! ```ignore
//! use rudo_gc::gc::sync::{LockGuard, LockOrder};
//!
//! fn safe_operation() {
//!     let _guard = LockGuard::new(LockOrder::LocalHeap);
//!     // Safe to acquire SegmentManager (same level) here
//!     // Safe to acquire GlobalMarkState (higher level) here
//! }
//! ```
//!
//! ## Lock Order Validation
//!
//! ```ignore
//! use rudo_gc::gc::sync::{acquire_lock, get_current_lock_level, LockOrder};
//!
//! fn multi_lock_operation() {
//!     let current_level = get_current_lock_level();
//!     acquire_lock(LockOrder::GlobalMarkState, current_level);
//!     let _mark_state = IncrementalMarkState::global();
//!     // ... GC operations
//! }
//! ```
//!
//! ## Thread Registry Access
//!
//! ```ignore
//! use rudo_gc::gc::sync::{acquire_lock, get_current_lock_level, LockOrder};
//!
//! fn thread_registry_operation() {
//!     let current_level = get_current_lock_level();
//!     // thread_registry() is level 2, validate after level 1
//!     acquire_lock(LockOrder::GlobalMarkState, current_level);
//!     let registry = thread_registry();
//!     // ... operations
//! }
//! ```
//!
//! ## Common Mistakes
//!
//! ### Wrong: Acquiring `GcRequest` while holding `LocalHeap`
//!
//! ```ignore,should_panic
//! fn wrong_order() {
//!     let _guard = LockGuard::new(LockOrder::GcRequest);  // Level 3
//!     let _manager = segment_manager(); // Level 1 - WRONG!
//! }
//! ```
//!
//! ### Correct: Acquiring in order
//!
//! ```ignore
//! fn correct_order() {
//!     let _manager = segment_manager();  // Level 1
//!     let _guard = LockGuard::new(LockOrder::GcRequest); // Level 3
//! }
//! ```

use std::cell::{Cell, RefCell};
use std::sync::atomic::AtomicBool;

const MAX_LOCK_DEPTH: usize = 16;

struct LockOrderState {
    stack: RefCell<Vec<u8>>,
    is_shutdown: Cell<bool>,
}

thread_local!(static LOCK_ORDER_STATE: LockOrderState = LockOrderState {
    stack: RefCell::new(Vec::with_capacity(MAX_LOCK_DEPTH)),
    is_shutdown: Cell::new(false),
});

/// Flag indicating whether the GC mark phase is currently in progress.
///
/// Used to prevent lazy sweeping during marking, which could cause race conditions
/// where marked objects are swept before the mark phase completes.
pub static GC_MARK_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Lock order tags for validation.
///
/// Each lock type has a unique order value. Locks must be acquired in
/// increasing level to prevent circular wait conditions.
///
/// # Lock Level Semantics
///
/// | Level | Lock Types | Semantics |
/// |-------|------------|-----------|
/// | 1 | LocalHeap, SegmentManager | Allocation locks - can be acquired in any order |
/// | 2 | GlobalMarkState | Coordination lock - must be after level 1 |
/// | 3 | GcRequest | Request lock - must be after level 2 |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LockOrder {
    /// `LocalHeap` per-thread allocation lock (level 1).
    /// Used for thread-local allocation operations.
    LocalHeap = 1,

    /// `SegmentManager` global memory management lock (level 1).
    /// Used for page allocation and large object management.
    /// Can be acquired before or after `LocalHeap` (both are level 1).
    SegmentManager = 2,

    /// `GlobalMarkState` mark phase coordination lock (level 2).
    /// Used for coordinating incremental and parallel marking.
    /// Must be acquired after all level 1 locks.
    GlobalMarkState = 3,

    /// `GcRequest` trigger and coordination lock (level 3).
    /// Used for GC request handling and thread coordination.
    /// Must be acquired after all level 2 locks.
    GcRequest = 4,
}

impl LockOrder {
    /// Get the order value for this lock type.
    ///
    /// Returns a unique value for each lock variant (1-4).
    /// Used for serialization and debugging.
    #[must_use]
    pub const fn order_value(self) -> u8 {
        self as u8
    }

    /// Get the conceptual lock level for this lock type.
    ///
    /// Lock levels define the acquisition hierarchy:
    /// - Level 1: `LocalHeap`, `SegmentManager` (allocation locks, can be acquired in any order)
    /// - Level 2: `GlobalMarkState` (coordination lock, must be after level 1)
    /// - Level 3: `GcRequest` (request lock, must be after level 2)
    #[must_use]
    #[allow(clippy::pedantic)]
    pub const fn level(self) -> u8 {
        match self {
            Self::LocalHeap => 1,
            Self::SegmentManager => 1,
            Self::GlobalMarkState => 2,
            Self::GcRequest => 3,
        }
    }
}

/// Lock order constants for use in lock acquisition.
///
/// These constants define the strict acquisition order to prevent deadlocks.
/// All locks must be acquired in increasing level.
pub mod lock_order {
    use super::LockOrder;

    /// `LocalHeap` per-thread allocation lock (level 1).
    #[allow(dead_code)]
    pub const LOCAL_HEAP: LockOrder = LockOrder::LocalHeap;

    /// `SegmentManager` global memory management lock (level 1).
    /// Used for page allocation and large object management.
    #[allow(dead_code)]
    pub const SEGMENT_MANAGER: LockOrder = LockOrder::SegmentManager;

    /// `GlobalMarkState` mark phase coordination lock (level 2).
    #[allow(dead_code)]
    pub const GLOBAL_MARK_STATE: LockOrder = LockOrder::GlobalMarkState;

    /// `GcRequest` trigger and coordination lock (level 3).
    #[allow(dead_code)]
    pub const GC_REQUEST: LockOrder = LockOrder::GcRequest;
}

/// Acquire a lock with ordering validation.
///
/// This function should be called when acquiring any lock protected by
/// the lock ordering discipline. It validates that the lock order is
/// correct in debug builds.
///
/// # Arguments
///
/// * `lock_tag` - The lock order tag for this lock type
/// * `current_level` - The current lock level held by this thread (1, 2, or 3)
///
/// # Panics
///
/// Panics in debug builds if `lock_tag` level is less than `current_level`.
#[inline]
#[allow(clippy::missing_const_for_fn)]
pub fn acquire_lock(lock_tag: LockOrder, current_level: u8) {
    #[cfg(debug_assertions)]
    {
        validate_lock_order(lock_tag, current_level);
    }
    let _ = (lock_tag, current_level); // Suppress unused warnings in release builds
}

/// Acquire a lock guard with automatic release order tracking.
///
/// This RAII-style guard automatically updates the minimum lock order
/// when acquired and provides validation in debug builds.
///
/// # Example
///
/// ```
/// use std::sync::{Mutex, MutexGuard};
/// use rudo_gc::gc::sync::{LockOrder, LockGuard};
///
/// fn example_function() {
///     let _guard = LockGuard::new(LockOrder::LocalHeap);
///     // Minimum lock level is now 1 (LocalHeap)
/// }
/// ```
#[must_use]
pub struct LockGuard {
    _tag: LockOrder,
}

impl LockGuard {
    /// Create a new lock guard for the given lock order.
    #[must_use = "LockGuard must be held for the duration of the critical section"]
    pub fn new(tag: LockOrder) -> Self {
        #[cfg(debug_assertions)]
        {
            let current_level = get_current_lock_level();
            validate_lock_order(tag, current_level);
            set_min_lock_order(tag);
        }
        Self { _tag: tag }
    }
}

#[cfg(debug_assertions)]
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = LOCK_ORDER_STATE.try_with(|state| {
            if !state.is_shutdown.get() {
                state.stack.borrow_mut().pop();
            }
        });
    }
}

/// Mark the current thread as shutting down.
///
/// After calling this function, lock order tracking is disabled.
/// This should be called during thread cleanup before the thread-local
/// storage is destroyed.
///
/// In release builds, this is a no-op.
#[inline]
#[allow(clippy::missing_const_for_fn)]
pub fn enter_thread_shutdown() {
    #[cfg(debug_assertions)]
    {
        let _ = LOCK_ORDER_STATE.try_with(|state| {
            state.is_shutdown.set(true);
        });
    }
}

/// Validate lock acquisition order in debug builds.
///
/// In release builds, this function is optimized away.
///
/// # Rules
///
/// Locks must be acquired in increasing level order:
/// - Level 1: `LocalHeap`, `SegmentManager` (can be acquired in any order relative to each other)
/// - Level 2: `GlobalMarkState` (must be after all level 1 locks)
/// - Level 3: `GcRequest` (must be after all level 2 locks)
///
/// Same-level acquisitions are always allowed (e.g., `LocalHeap` → `SegmentManager`).
/// Higher-level acquisitions are allowed.
/// Lower-level acquisitions (e.g., `GlobalMarkState` → `LocalHeap`) are forbidden.
///
/// # Panics
///
/// Panics in debug builds if the order violation is detected.
#[inline]
#[allow(clippy::format_in_format_args)]
#[cfg(debug_assertions)]
pub fn validate_lock_order(tag: LockOrder, current_level: u8) {
    let same_level = tag.level() == current_level;
    let is_downgrade = tag.level() < current_level;

    debug_assert!(
        same_level || !is_downgrade,
        "Lock ordering violation: {} (level {}) cannot be acquired while holding lock level {}. Downgrades are not allowed.",
        format!("{:?}", tag),
        tag.level(),
        current_level
    );
}

#[inline]
#[cfg(not(debug_assertions))]
pub fn validate_lock_order(_tag: LockOrder, _current_level: u8) {
    // No-op in release builds
}

/// Store a lock level in the thread-local stack.
///
/// Pushes the lock level for later validation of lock acquisition order.
#[inline]
#[allow(clippy::missing_const_for_fn)]
pub fn set_min_lock_order(order: LockOrder) {
    #[cfg(debug_assertions)]
    {
        let _ = LOCK_ORDER_STATE.try_with(|state| {
            if state.is_shutdown.get() {
                return;
            }
            state.stack.borrow_mut().push(order.level());
        });
    }
    let _ = order;
}

/// Get the current maximum lock level held by this thread.
///
/// Returns the highest lock level (0, 1, 2, or 3) currently held.
/// Returns 0 when no locks are held.
/// Used for validating lock acquisition order.
///
/// In debug builds, this function accesses thread-local storage.
/// During thread shutdown, the thread-local may be destroyed,
/// so we handle errors defensively and return 0 as a safe default.
#[inline]
#[cfg(debug_assertions)]
#[must_use]
pub fn get_current_lock_level() -> u8 {
    LOCK_ORDER_STATE
        .try_with(|state| {
            if state.is_shutdown.get() {
                return 0;
            }
            let stack = state.stack.borrow();
            if stack.is_empty() {
                return 0;
            }
            stack.iter().copied().max().unwrap_or(0)
        })
        .unwrap_or(0)
}

/// No-op in release builds (returns 0, allowing any lock acquisition).
#[inline]
#[cfg(not(debug_assertions))]
#[must_use]
pub fn get_current_lock_level() -> u8 {
    0
}

#[cfg(test)]
mod tests {
    use super::{LockGuard, LockOrder};

    #[test]
    fn test_lock_order_values() {
        assert_eq!(LockOrder::LocalHeap.order_value(), 1);
        assert_eq!(LockOrder::SegmentManager.order_value(), 2);
        assert_eq!(LockOrder::GlobalMarkState.order_value(), 3);
        assert_eq!(LockOrder::GcRequest.order_value(), 4);
    }

    #[test]
    fn test_lock_order_levels() {
        assert_eq!(LockOrder::LocalHeap.level(), 1);
        assert_eq!(LockOrder::SegmentManager.level(), 1);
        assert_eq!(LockOrder::GlobalMarkState.level(), 2);
        assert_eq!(LockOrder::GcRequest.level(), 3);
    }

    #[test]
    fn test_lock_order_comparison() {
        assert!(LockOrder::LocalHeap.level() < LockOrder::GlobalMarkState.level());
        assert!(LockOrder::GlobalMarkState.level() < LockOrder::GcRequest.level());
    }

    #[test]
    fn test_lock_guard_valid_order_local_heap() {
        let _guard1 = LockGuard::new(LockOrder::LocalHeap);
        let _guard2 = LockGuard::new(LockOrder::SegmentManager);
        let _guard3 = LockGuard::new(LockOrder::GlobalMarkState);
        let _guard4 = LockGuard::new(LockOrder::GcRequest);
    }

    #[test]
    fn test_lock_guard_valid_order_segment_manager() {
        let _guard1 = LockGuard::new(LockOrder::SegmentManager);
        let _guard2 = LockGuard::new(LockOrder::GlobalMarkState);
        let _guard3 = LockGuard::new(LockOrder::GcRequest);
    }

    #[test]
    fn test_lock_guard_mixed_level_1_order() {
        let _guard1 = LockGuard::new(LockOrder::SegmentManager);
        let _guard2 = LockGuard::new(LockOrder::LocalHeap);
    }

    #[test]
    #[should_panic(expected = "Lock ordering violation")]
    fn test_lock_guard_invalid_order_should_panic() {
        let _guard1 = LockGuard::new(LockOrder::GlobalMarkState);
        let _guard2 = LockGuard::new(LockOrder::LocalHeap);
    }

    #[test]
    fn test_lock_guard_acquire_in_order() {
        let _guard1 = LockGuard::new(LockOrder::LocalHeap);
        let _guard2 = LockGuard::new(LockOrder::SegmentManager);
        let _guard3 = LockGuard::new(LockOrder::GlobalMarkState);
        let _guard4 = LockGuard::new(LockOrder::GcRequest);
    }

    #[test]
    #[should_panic(expected = "Lock ordering violation")]
    fn test_lock_guard_nested_drop_then_lower_order_should_panic_if_bug_exists() {
        let _guard1 = LockGuard::new(LockOrder::GlobalMarkState);
        {
            let _guard2 = LockGuard::new(LockOrder::GcRequest);
        }
        let _guard3 = LockGuard::new(LockOrder::LocalHeap);
    }

    #[test]
    fn test_lock_guard_state_restoration_after_drop() {
        {
            let _guard1 = LockGuard::new(LockOrder::GlobalMarkState);
            {
                let _guard2 = LockGuard::new(LockOrder::GcRequest);
            }
        }
        let _guard3 = LockGuard::new(LockOrder::LocalHeap);
    }

    #[test]
    fn test_lock_guard_nested_scopes_with_early_drops() {
        let _guard1 = LockGuard::new(LockOrder::LocalHeap);
        {
            let _guard2 = LockGuard::new(LockOrder::SegmentManager);
            {
                let _guard3 = LockGuard::new(LockOrder::GlobalMarkState);
            }
        }
        let _guard4 = LockGuard::new(LockOrder::GlobalMarkState);
    }

    #[test]
    fn test_lock_guard_multiple_nested_scopes() {
        {
            let _guard1 = LockGuard::new(LockOrder::LocalHeap);
            {
                let _guard2 = LockGuard::new(LockOrder::SegmentManager);
                {
                    let _guard3 = LockGuard::new(LockOrder::GlobalMarkState);
                }
            }
        }
        let _guard4 = LockGuard::new(LockOrder::LocalHeap);
        let _guard5 = LockGuard::new(LockOrder::SegmentManager);
        let _guard6 = LockGuard::new(LockOrder::GlobalMarkState);
    }

    #[test]
    fn test_segment_manager_after_local_heap() {
        let _guard1 = LockGuard::new(LockOrder::LocalHeap);
        let _guard2 = LockGuard::new(LockOrder::SegmentManager);
    }

    #[test]
    fn test_segment_manager_then_gc_request() {
        let _guard1 = LockGuard::new(LockOrder::SegmentManager);
        let _guard2 = LockGuard::new(LockOrder::GcRequest);
    }

    #[test]
    fn test_lock_guard_mixed_order_1() {
        let _guard1 = LockGuard::new(LockOrder::SegmentManager);
        let _guard2 = LockGuard::new(LockOrder::LocalHeap);
        let _guard3 = LockGuard::new(LockOrder::GlobalMarkState);
    }

    #[test]
    fn test_lock_guard_mixed_order_2() {
        let _guard1 = LockGuard::new(LockOrder::LocalHeap);
        let _guard2 = LockGuard::new(LockOrder::SegmentManager);
        let _guard3 = LockGuard::new(LockOrder::GcRequest);
    }

    #[test]
    #[should_panic(expected = "Lock ordering violation")]
    fn test_cannot_acquire_local_heap_after_global_mark_state() {
        let _guard1 = LockGuard::new(LockOrder::GlobalMarkState);
        let _guard2 = LockGuard::new(LockOrder::LocalHeap);
    }

    #[test]
    #[should_panic(expected = "Lock ordering violation")]
    fn test_cannot_acquire_segment_manager_after_gc_request() {
        let _guard1 = LockGuard::new(LockOrder::GcRequest);
        let _guard2 = LockGuard::new(LockOrder::SegmentManager);
    }
}
