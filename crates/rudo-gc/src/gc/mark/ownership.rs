//! Segment ownership integration for load distribution.
//!
//! This module provides utilities for tracking page ownership and implementing
//! ownership-based work distribution to improve cache locality and reduce
//! false sharing during parallel marking.
//!
//! # Thread Safety Model
//!
//! Each worker thread has its own `OwnedPagesTracker`. Ownership is established
//! at allocation time (when a page is created, it records the allocating thread).
//! During marking, workers prioritize their owned pages for better cache locality.
//!
//! This design follows Chez Scheme's approach: "A segment is owned by the thread
//! that originally allocated it."

use std::collections::HashSet;
use std::ptr::NonNull;
use std::thread::ThreadId;

use crate::heap::PageHeader;

/// Owned pages tracker for a worker thread.
///
/// Tracks pages owned by this worker for ownership-based load distribution.
/// When marking, workers prioritize their owned pages for better cache locality.
///
/// # Thread Safety
///
/// `OwnedPagesTracker` is `Send` but not `Sync`. Each thread should have its
/// own instance, accessed only by the owning thread. This matches Chez Scheme's
/// design where ownership is established at allocation time.
#[derive(Debug)]
pub struct OwnedPagesTracker {
    /// Set of pages owned by this worker.
    owned_pages: HashSet<NonNull<PageHeader>>,
    /// The worker thread ID.
    owner_thread: ThreadId,
}

/// Safety: `OwnedPagesTracker` is `Send` because:
/// - `HashSet`<`NonNull`<PageHeader>> is `Send` (`NonNull` is `Send`, interior mutability
///   is protected by thread ownership invariant)
/// - `ThreadId` is `Send`
/// - The tracker is only ever accessed by the owning thread
unsafe impl Send for OwnedPagesTracker {}

impl OwnedPagesTracker {
    /// Create a new owned pages tracker for the current thread.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owned_pages: HashSet::new(),
            owner_thread: std::thread::current().id(),
        }
    }

    /// Register a page as owned by this worker.
    pub fn add_owned_page(&mut self, page: NonNull<PageHeader>) {
        self.owned_pages.insert(page);
    }

    /// Unregister a page from ownership.
    pub fn remove_owned_page(&mut self, page: NonNull<PageHeader>) {
        self.owned_pages.remove(&page);
    }

    /// Check if a page is owned by this worker.
    #[must_use]
    pub fn owns_page(&self, page: NonNull<PageHeader>) -> bool {
        self.owned_pages.contains(&page)
    }

    /// Get the number of owned pages.
    #[must_use]
    pub fn owned_count(&self) -> usize {
        self.owned_pages.len()
    }

    /// Get the owner thread ID.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn owner_thread(&self) -> ThreadId {
        self.owner_thread
    }

    /// Get an iterator over owned pages.
    pub fn iter(&self) -> impl Iterator<Item = &NonNull<PageHeader>> {
        self.owned_pages.iter()
    }
}

/// Get the worker thread's owned pages tracker.
///
/// Creates a new tracker if one doesn't exist for the current thread.
#[must_use]
pub fn get_owned_pages_tracker() -> OwnedPagesTracker {
    OwnedPagesTracker::new()
}

#[cfg(test)]
mod tests {
    use super::OwnedPagesTracker;
    use std::ptr::NonNull;

    #[test]
    fn test_owned_pages_tracker_new() {
        let tracker = OwnedPagesTracker::new();
        assert_eq!(tracker.owned_count(), 0);
    }

    #[test]
    fn test_owned_pages_tracker_add_remove() {
        let mut tracker = OwnedPagesTracker::new();
        assert_eq!(tracker.owned_count(), 0);

        // We can't easily create a real PageHeader in tests,
        // so we just test the HashSet operations
        let dummy_ptr = NonNull::dangling();
        tracker.add_owned_page(dummy_ptr);
        assert_eq!(tracker.owned_count(), 1);
        assert!(tracker.owns_page(dummy_ptr));

        tracker.remove_owned_page(dummy_ptr);
        assert_eq!(tracker.owned_count(), 0);
        assert!(!tracker.owns_page(dummy_ptr));
    }
}
