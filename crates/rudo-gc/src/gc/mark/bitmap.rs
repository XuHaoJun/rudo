//! Mark bitmap implementation for page-level object marking.
//!
//! This module provides a mark bitmap that records object liveness using one bit
//! per pointer-sized unit, replacing per-object forwarding pointers.

use std::sync::atomic::{AtomicUsize, Ordering};

/// A page-level bitmap for marking objects during garbage collection.
///
/// The bitmap uses one bit per pointer-sized unit (8 bytes on 64-bit systems).
/// For a 4KB page with 512 pointer slots, the bitmap uses 64 bytes (512 bits),
/// compared to 4096 bytes for per-object forwarding pointers.
///
/// # Example
///
/// ```
/// use rudo_gc::gc::mark::MarkBitmap;
///
/// let bitmap = MarkBitmap::new(512);
/// assert_eq!(bitmap.capacity(), 512);
/// assert!(!bitmap.is_marked(0));
///
/// bitmap.mark(0);
/// assert!(bitmap.is_marked(0));
/// ```
#[derive(Debug)]
pub struct MarkBitmap {
    /// Bitmap storage, one bit per pointer-sized unit.
    bitmap: Vec<u64>,
    /// Number of pointer slots in the page.
    capacity: usize,
    /// Number of marked slots (atomic for parallel access).
    marked_count: AtomicUsize,
}

impl MarkBitmap {
    /// Create a new mark bitmap with the given capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is not aligned to 64 (pointer slots per u64 word).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(
            capacity % 64 == 0,
            "MarkBitmap capacity must be aligned to 64"
        );
        let bits = (capacity + 63).div_ceil(64);
        Self {
            bitmap: vec![0u64; bits],
            capacity,
            marked_count: AtomicUsize::new(0),
        }
    }

    /// Get the capacity of the bitmap (number of pointer slots).
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get the number of marked slots.
    #[must_use]
    pub fn marked_count(&self) -> usize {
        self.marked_count.load(Ordering::Relaxed)
    }

    /// Mark a slot as visited.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `slot_index` is within bounds.
    pub unsafe fn mark(&mut self, slot_index: usize) {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        let mask = 1u64 << bit;
        let prev = self.bitmap[word];
        self.bitmap[word] |= mask;
        // Only increment if bit was not already set (idempotent marking)
        if prev & mask == 0 {
            self.marked_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Check if a slot is marked.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `slot_index` is within bounds.
    #[must_use]
    pub unsafe fn is_marked(&self, slot_index: usize) -> bool {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        (self.bitmap[word] >> bit) & 1 != 0
    }

    /// Clear all marks for reuse.
    pub fn clear(&mut self) {
        for word in &mut self.bitmap {
            *word = 0;
        }
        self.marked_count.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::MarkBitmap;

    #[test]
    fn test_mark_bitmap_new() {
        let bitmap = MarkBitmap::new(512);
        assert_eq!(bitmap.capacity(), 512);
        assert_eq!(bitmap.marked_count(), 0);
    }

    #[test]
    fn test_mark_bitmap_mark_is_marked() {
        let mut bitmap = MarkBitmap::new(512);

        assert!(!unsafe { bitmap.is_marked(0) });
        assert!(!unsafe { bitmap.is_marked(63) });

        unsafe { bitmap.mark(0) };
        unsafe { bitmap.mark(63) };

        assert!(unsafe { bitmap.is_marked(0) });
        assert!(unsafe { bitmap.is_marked(63) });
        assert!(!unsafe { bitmap.is_marked(1) });
    }

    #[test]
    fn test_mark_bitmap_clear() {
        let mut bitmap = MarkBitmap::new(512);

        unsafe { bitmap.mark(0) };
        unsafe { bitmap.mark(100) };
        assert_eq!(bitmap.marked_count(), 2);

        bitmap.clear();
        assert_eq!(bitmap.marked_count(), 0);
        assert!(!unsafe { bitmap.is_marked(0) });
    }

    #[test]
    fn test_mark_bitmap_idempotent() {
        let mut bitmap = MarkBitmap::new(512);

        unsafe { bitmap.mark(0) };
        unsafe { bitmap.mark(0) };
        unsafe { bitmap.mark(0) };

        assert_eq!(bitmap.marked_count(), 1);
    }
}
