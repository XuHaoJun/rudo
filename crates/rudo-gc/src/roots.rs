//! Root tracking using a shadow stack.
//!
//! This module implements root tracking for the garbage collector.
//! Roots are `Gc` pointers that are directly accessible from the stack.

use std::cell::RefCell;
use std::ptr::NonNull;

use crate::ptr::GcBox;

// ============================================================================
// ShadowStack - Root tracking structure
// ============================================================================

/// A shadow stack for tracking GC roots.
///
/// This structure maintains a list of all active `Gc<T>` pointers
/// that should be treated as roots during garbage collection.
pub struct ShadowStack {
    /// Type-erased pointers to all active roots.
    roots: Vec<NonNull<GcBox<()>>>,
    /// Stack of frame markers for scope-based rooting (future optimization).
    #[allow(dead_code)]
    frame_markers: Vec<usize>,
}

impl ShadowStack {
    /// Create a new empty shadow stack.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            roots: Vec::new(),
            frame_markers: Vec::new(),
        }
    }

    /// Register a new root.
    pub fn push(&mut self, ptr: NonNull<GcBox<()>>) {
        self.roots.push(ptr);
    }

    /// Unregister a root.
    pub fn pop(&mut self, ptr: NonNull<GcBox<()>>) {
        if let Some(pos) = self.roots.iter().position(|&r| r == ptr) {
            self.roots.swap_remove(pos);
        }
    }

    /// Get the number of roots.
    #[allow(dead_code)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Check if there are no roots.
    #[allow(dead_code)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Iterate over all roots.
    pub fn iter(&self) -> impl Iterator<Item = NonNull<GcBox<()>>> + '_ {
        self.roots.iter().copied()
    }

    /// Clear all roots.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.roots.clear();
        self.frame_markers.clear();
    }

    /// Push a frame marker (for scope-based rooting).
    #[allow(dead_code)]
    pub fn push_frame(&mut self) {
        self.frame_markers.push(self.roots.len());
    }

    /// Pop a frame marker and remove all roots added since.
    #[allow(dead_code)]
    pub fn pop_frame(&mut self) {
        if let Some(marker) = self.frame_markers.pop() {
            self.roots.truncate(marker);
        }
    }
}

impl Default for ShadowStack {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Thread-local root access
// ============================================================================

thread_local! {
    /// Thread-local shadow stack for root tracking.
    pub static ROOTS: RefCell<ShadowStack> = const { RefCell::new(ShadowStack::new()) };
}

/// Execute a function with access to the shadow stack.
pub fn with_roots<F, R>(f: F) -> R
where
    F: FnOnce(&mut ShadowStack) -> R,
{
    ROOTS.with(|roots| f(&mut roots.borrow_mut()))
}

// ============================================================================
// Scope-based rooting (future optimization)
// ============================================================================

/// A guard for scope-based root management.
///
/// When dropped, all roots registered since this guard was created
/// are automatically unregistered.
#[allow(dead_code)]
pub struct RootScope {
    /// Dummy field to prevent construction outside this module.
    _private: (),
}

impl RootScope {
    /// Create a new root scope.
    #[allow(dead_code)]
    #[must_use]
    pub fn new() -> Self {
        ROOTS.with(|roots| {
            roots.borrow_mut().push_frame();
        });
        Self { _private: () }
    }
}

impl Default for RootScope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RootScope {
    fn drop(&mut self) {
        ROOTS.with(|roots| {
            roots.borrow_mut().pop_frame();
        });
    }
}
