#![allow(
    clippy::explicit_iter_loop,
    clippy::too_many_lines,
    clippy::missing_panics_doc
)]

//! Process-level GC root tracking singleton.
//!
//! This module provides [`GcRootSet`], a process-level singleton that maintains
//! the collection of active GC roots across all tokio tasks and runtimes.

use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Process-level singleton for tracking GC roots across all tokio contexts.
///
/// `GcRootSet` maintains the set of active GC roots and provides methods for
/// registering and unregistering roots. It uses atomic operations for thread-safe
/// access from any tokio thread.
///
/// The singleton is initialized on first access via [`GcRootSet::global()`].
#[derive(Debug)]
pub struct GcRootSet {
    roots: Mutex<Vec<usize>>,
    dirty: AtomicBool,
}

impl GcRootSet {
    /// Returns a reference to the global `GcRootSet` singleton.
    ///
    /// The singleton is created on first access using `OnceLock`.
    #[must_use]
    #[allow(clippy::use_self)]
    pub fn global() -> &'static Self {
        GLOBAL.get_or_init(Self::new)
    }

    /// Creates a new `GcRootSet`.
    const fn new() -> Self {
        Self {
            roots: Mutex::new(Vec::new()),
            dirty: AtomicBool::new(false),
        }
    }

    /// Registers a pointer as a GC root.
    ///
    /// If the pointer is already registered, this is a no-op.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Arguments
    ///
    /// * `ptr` - The raw pointer address to register
    #[allow(clippy::significant_drop_tightening)]
    pub fn register(&self, ptr: usize) {
        let mut roots = self.roots.lock().unwrap();
        if !roots.contains(&ptr) {
            roots.push(ptr);
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// Unregisters a pointer from the GC root set.
    ///
    /// If the pointer is not registered, this is a no-op.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Arguments
    ///
    /// * `ptr` - The raw pointer address to unregister
    pub fn unregister(&self, ptr: usize) {
        let mut roots = self.roots.lock().unwrap();
        let was_present = roots.contains(&ptr);
        if was_present {
            roots.retain(|&p| p != ptr);
        }
        drop(roots);

        if was_present {
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// Returns the number of currently registered roots.
    #[inline]
    pub fn len(&self) -> usize {
        self.roots.lock().unwrap().len()
    }

    /// Returns whether the root set is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.roots.lock().unwrap().is_empty()
    }

    /// Takes a snapshot of the current roots.
    ///
    /// This atomically captures the current root set and clears the dirty flag.
    /// The returned vector contains all currently registered root pointers that
    /// are valid `GcBox` pointers in the given heap.
    ///
    /// Invalid pointers (non-GcBox addresses) are silently filtered out.
    ///
    /// # Panics
    ///
    /// This function panics if the internal mutex is poisoned.
    ///
    /// # Arguments
    ///
    /// * `heap` - The local heap to validate pointers against
    ///
    /// # Returns
    ///
    /// A vector of valid root pointer addresses
    pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
        let roots = self.roots.lock().unwrap();
        let valid_roots: Vec<usize> = roots
            .iter()
            .filter(|&&ptr| {
                // SAFETY: find_gc_box_from_ptr performs range and alignment checks.
                // If it returns Some, ptr is a valid GcBox.
                unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
            })
            .copied()
            .collect();
        drop(roots);
        self.dirty.store(false, Ordering::Release);
        valid_roots
    }

    /// Returns whether the root set has been modified since last snapshot.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Clears the dirty flag.
    ///
    /// This is primarily useful for testing.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    #[inline]
    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    /// Checks if a pointer is currently registered as a root.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Arguments
    ///
    /// * `ptr` - The raw pointer address to check
    ///
    /// # Returns
    ///
    /// `true` if the pointer is registered, `false` otherwise
    #[inline]
    pub fn is_registered(&self, ptr: usize) -> bool {
        let roots = self.roots.lock().unwrap();
        roots.contains(&ptr)
    }

    /// Clears all registered roots.
    ///
    /// This is primarily useful for testing.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    pub fn clear(&self) {
        self.roots.lock().unwrap().clear();
        self.dirty.store(true, Ordering::Release);
    }
}

static GLOBAL: OnceLock<GcRootSet> = OnceLock::new();

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;

    #[test]
    fn test_singleton_creation() {
        let set1 = GcRootSet::global();
        let set2 = GcRootSet::global();
        assert!(std::ptr::eq(set1, set2));
    }

    #[test]
    fn test_register_unregister() {
        let set = GcRootSet::global();
        set.clear();

        assert!(set.is_empty());

        set.register(0x1234);
        assert_eq!(set.len(), 1);
        assert!(set.is_registered(0x1234));
        assert!(set.is_dirty());

        set.register(0x1234); // Duplicate - should not increment
        assert_eq!(set.len(), 1);

        set.unregister(0x1234);
        assert!(set.is_empty());
        assert!(!set.is_registered(0x1234));
    }

    #[test]
    fn test_snapshot() {
        let set = GcRootSet::global();
        set.clear();

        set.register(0x1000);
        set.register(0x2000);

        assert_eq!(set.len(), 2);
        assert!(set.is_dirty());
        set.clear_dirty();
        assert!(!set.is_dirty());
    }

    #[test]
    fn test_clear() {
        let set = GcRootSet::global();
        set.register(0x1000);
        set.register(0x2000);

        set.clear();

        assert!(set.is_empty());
        assert!(!set.is_registered(0x1000));
        assert!(!set.is_registered(0x2000));
    }
}
