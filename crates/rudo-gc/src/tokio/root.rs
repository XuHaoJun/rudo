#![allow(
    clippy::explicit_iter_loop,
    clippy::too_many_lines,
    clippy::missing_panics_doc
)]

//! Process-level GC root tracking singleton.
//!
//! This module provides [`GcRootSet`], a process-level singleton that maintains
//! the collection of active GC roots across all tokio tasks and runtimes.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Entry storing count and generation for slot reuse detection.
/// Generation is read from the `GcBox` when the root is registered,
/// and verified during snapshot to detect slot reuse.
type RootEntry = (usize, u32);

/// Process-level singleton for tracking GC roots across all tokio contexts.
///
/// `GcRootSet` maintains the set of active GC roots and provides methods for
/// registering and unregistering roots. It uses atomic operations for thread-safe
/// access from any tokio thread.
///
/// The singleton is initialized on first access via [`GcRootSet::global()`].
///
/// # Lock Ordering (bug203)
/// The internal `roots` mutex is exempt from the GC `LockOrder` validation system.
/// `GcRootSet` is used by tokio tasks for root registration and is independent of
/// the core GC lock hierarchy (`LocalHeap`, `GlobalMarkState`, `GcRequest`).
#[derive(Debug)]
pub struct GcRootSet {
    roots: Mutex<HashMap<usize, RootEntry>>,
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
    fn new() -> Self {
        Self {
            roots: Mutex::new(HashMap::default()),
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
        use std::collections::hash_map::Entry;
        let mut roots = self.roots.lock().unwrap();

        match roots.entry(ptr) {
            Entry::Vacant(v) => {
                let generation = crate::heap::try_with_heap(|heap| unsafe {
                    crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8)
                        .map_or(0u32, |gc_box| gc_box.as_ref().generation())
                })
                .unwrap_or(0);
                v.insert((1, generation));
            }
            Entry::Occupied(o) => {
                let current_generation = crate::heap::try_with_heap(|heap| unsafe {
                    crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8)
                        .map_or(0u32, |gc_box| gc_box.as_ref().generation())
                })
                .unwrap_or(0);

                let entry = o.into_mut();
                if entry.1 == current_generation {
                    entry.0 += 1;
                } else {
                    entry.0 = 1;
                    entry.1 = current_generation;
                }
            }
        }

        self.dirty.store(true, Ordering::Release);
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
        let needs_dirty = {
            let mut roots = self.roots.lock().unwrap();
            if let Some(entry) = roots.get_mut(&ptr) {
                entry.0 -= 1;
                if entry.0 == 0 {
                    roots.remove(&ptr);
                }
                true
            } else {
                false
            }
        };
        if needs_dirty {
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
    /// are valid `GcBox` pointers in the given heap and have matching generation.
    ///
    /// Invalid pointers (non-GcBox addresses) or pointers to reused slots are silently filtered out.
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
            .filter(|(&ptr, &(_, stored_generation))| {
                // SAFETY: find_gc_box_from_ptr performs range and alignment checks.
                // If it returns Some, ptr is a valid GcBox.
                let Some(gc_box) =
                    (unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) })
                else {
                    return false;
                };
                // FIX bug514: Check generation to detect slot reuse after sweep.
                // If the slot was swept and reallocated to a new object, the generation
                // will have changed and we should not treat this as a valid root.
                let current_generation = unsafe { (*gc_box.as_ptr()).generation() };
                current_generation == stored_generation
            })
            .map(|(&ptr, _)| ptr)
            .collect();
        // Clear dirty while still holding the lock so concurrent register/unregister
        // operations cannot have their updates overwritten.
        self.dirty.store(false, Ordering::Release);
        drop(roots);
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
    /// # TOCTOU race
    ///
    /// This function has a time-of-check-time-of-use (TOCTOU) race: the lock is
    /// released before returning, so the result may be stale by the time the
    /// caller uses it. If another thread calls `unregister(ptr)` between the
    /// check and the use, the caller may incorrectly treat the pointer as a
    /// root when it is not, potentially leading to use-after-free.
    ///
    /// # Safety
    ///
    /// The caller must ensure that between the call and any use of the result,
    /// no other thread calls `unregister(ptr)` on the same pointer. In
    /// single-threaded contexts this is trivially satisfied.
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
    /// `true` if the pointer was registered at call time, `false` otherwise
    #[inline]
    pub unsafe fn is_registered(&self, ptr: usize) -> bool {
        let roots = self.roots.lock().unwrap();
        roots.contains_key(&ptr)
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

    /// Shuts down the global root set, clearing all registered roots.
    ///
    /// This is intended for use during process shutdown to ensure all
    /// GC roots are released. After shutdown, calling `global()` will
    /// create a new root set.
    ///
    /// # Safety
    ///
    /// This must only be called when no GC operations are in progress,
    /// as concurrent access during shutdown is unsafe.
    pub fn shutdown() {
        if let Some(set) = GLOBAL.get() {
            set.roots.lock().unwrap().clear();
            set.dirty.store(false, Ordering::Release);
        }
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
        assert!(unsafe { set.is_registered(0x1234) });
        assert!(set.is_dirty());

        set.register(0x1234); // Duplicate - increments ref count to 2
        assert_eq!(set.len(), 1); // Still 1 unique key

        set.unregister(0x1234); // Decrements count to 1, not removed yet
        assert!(!set.is_empty()); // Count is 1

        set.unregister(0x1234); // Decrements count to 0, removes key
        assert!(set.is_empty()); // Now empty
        assert!(!unsafe { set.is_registered(0x1234) });
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
        assert!(!unsafe { set.is_registered(0x1000) });
        assert!(!unsafe { set.is_registered(0x2000) });
    }
}
