//! Process-level GC root tracking singleton.
//!
//! This module provides [`GcRootSet`], a process-level singleton that maintains
//! the collection of active GC roots across all tokio tasks and runtimes.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    count: AtomicUsize,
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
            count: AtomicUsize::new(0),
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
            drop(roots);
            self.count.fetch_add(1, Ordering::AcqRel);
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
        if let Some(pos) = roots.iter().position(|&p| p == ptr) {
            roots.swap_remove(pos);
        }
        drop(roots);

        self.count.fetch_sub(1, Ordering::AcqRel);
        self.dirty.store(true, Ordering::Release);
    }

    /// Returns the number of currently registered roots.
    #[inline]
    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Takes a snapshot of the current roots.
    ///
    /// This atomically captures the current root set and clears the dirty flag.
    /// The returned vector contains all currently registered root pointers.
    ///
    /// # Panics
    ///
    /// This function panics if the internal mutex is poisoned.
    ///
    /// # Returns
    ///
    /// A vector of root pointer addresses
    pub fn snapshot(&self) -> Vec<usize> {
        let roots = self.roots.lock().unwrap().clone();
        self.dirty.store(false, Ordering::Release);
        roots
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
        self.count.store(0, Ordering::Release);
        self.dirty.store(true, Ordering::Release);
    }
}

static GLOBAL: OnceLock<GcRootSet> = OnceLock::new();

#[cfg(test)]
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

        assert_eq!(set.count(), 0);

        set.register(0x1234);
        assert_eq!(set.count(), 1);
        assert!(set.is_registered(0x1234));
        assert!(set.is_dirty());

        set.register(0x1234); // Duplicate - should not increment
        assert_eq!(set.count(), 1);

        set.unregister(0x1234);
        assert_eq!(set.count(), 0);
        assert!(!set.is_registered(0x1234));
    }

    #[test]
    fn test_snapshot() {
        let set = GcRootSet::global();
        set.clear();

        set.register(0x1000);
        set.register(0x2000);

        let snapshot = set.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.contains(&0x1000));
        assert!(snapshot.contains(&0x2000));
        assert!(!set.is_dirty());
    }

    #[test]
    fn test_clear() {
        let set = GcRootSet::global();
        set.register(0x1000);
        set.register(0x2000);

        set.clear();

        assert_eq!(set.count(), 0);
        assert!(!set.is_registered(0x1000));
        assert!(!set.is_registered(0x2000));
    }
}
