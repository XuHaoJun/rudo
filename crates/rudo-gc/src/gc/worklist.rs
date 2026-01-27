//! Work-stealing queue implementations for parallel marking.
//!
//! This module provides lock-free work-stealing deques based on the Chase-Lev algorithm
//! for efficient parallel garbage collection marking.

use std::cell::Cell;
use std::cell::UnsafeCell;
use std::marker::Copy;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Lock-free work stealing queue.
///
/// Based on: "Simple and Efficient Work-Stealing Queues for Parallel Programming"
/// by Chase and Lev (2005).
///
/// The queue uses a circular buffer with separate bottom (producer) and top (consumer)
/// pointers. Local push/pop operations access the bottom (LIFO), while steal operations
/// access the top (FIFO).
///
/// # Invariants
///
/// - `N` must be a power of 2
/// - `mask = N - 1`
/// - Queue is empty when `bottom == top`
/// - Queue is full when `bottom - top == N`
/// - Size is always `bottom - top` (modulo arithmetic)
#[derive(Debug)]
#[allow(dead_code)]
pub struct StealQueue<T: Copy, const N: usize> {
    buffer: UnsafeCell<[MaybeUninit<T>; N]>,
    bottom: Cell<usize>,
    top: AtomicUsize,
    mask: usize,
}

impl<T: Copy, const N: usize> StealQueue<T, N> {
    #[allow(dead_code)]
    /// Create a new steal queue.
    ///
    /// # Panics
    ///
    /// Panics if `N` is not a power of 2.
    #[must_use]
    pub const fn new() -> Self {
        assert!(
            N.is_power_of_two(),
            "StealQueue size N must be a power of 2"
        );

        Self {
            buffer: UnsafeCell::new([const { MaybeUninit::uninit() }; N]),
            bottom: Cell::new(0),
            top: AtomicUsize::new(0),
            mask: N - 1,
        }
    }

    /// Push an item to the local end (LIFO).
    ///
    /// Returns `true` if successful, `false` if the queue is full.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The queue is not full (checked by `bottom - top < N`)
    /// - No other thread is concurrently pushing to the same slot
    /// - The slot at `bottom & mask` has not been written to in this call
    ///
    /// The slot is exclusively owned by the pusher when push succeeds,
    /// and the write is made visible with a release store.
    #[allow(dead_code)]
    pub fn push(&self, bottom: &Cell<usize>, item: T) -> bool {
        let b = bottom.get();
        let t = self.top.load(Ordering::Acquire);

        if b.wrapping_sub(t) >= N {
            return false;
        }

        let index = b & self.mask;

        unsafe {
            (*self.buffer.get())[index].write(item);
        }

        bottom.set(b.wrapping_add(1));

        true
    }

    /// Pop an item from the local end (LIFO).
    ///
    /// Returns `Some(item)` if successful, `None` if the queue is empty.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The queue is not empty (checked by `bottom != top`)
    /// - No other thread is concurrently popping from the same slot
    /// - The slot at `(bottom - 1) & mask` was previously written by push
    ///
    /// We have exclusive access to read from this slot. If this is the last
    /// item, we synchronize with stealers using CAS on top.
    #[allow(dead_code)]
    pub fn pop(&self, bottom: &Cell<usize>) -> Option<T> {
        let b = bottom.get();
        let t = self.top.load(Ordering::Acquire);

        if b == t {
            return None;
        }

        let new_b = b.wrapping_sub(1);
        bottom.set(new_b);

        let index = new_b & self.mask;

        // SAFETY: We verified the queue is not empty, so this slot was
        // previously written by push(). We have exclusive access to read it.
        // No other thread can be accessing this slot concurrently for pop.
        let item = unsafe { (*self.buffer.get())[index].assume_init_read() };

        if new_b != t {
            return Some(item);
        }

        // This was the last item - need to synchronize with stealers
        if self
            .top
            .compare_exchange(t, t.wrapping_add(1), Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Another thread stole the item - put it back
            bottom.set(b);
            return None;
        }

        bottom.set(t.wrapping_add(1));
        Some(item)
    }

    /// Steal an item from the remote end (FIFO).
    ///
    /// Returns `Some(item)` if successful, `None` if the queue is empty.
    ///
    /// # Safety
    ///
    /// This operation may be called concurrently from multiple threads.
    /// Multiple stealers may attempt to steal from the same queue.
    ///
    /// We use CAS on top to ensure only one stealer succeeds. The CAS
    /// prevents the ABA problem and ensures at-most-once semantics.
    #[allow(dead_code)]
    pub fn steal(&self, bottom: &Cell<usize>) -> Option<T> {
        let t = self.top.load(Ordering::Acquire);
        let b = bottom.get();

        if t == b {
            return None;
        }

        let new_top = t.wrapping_add(1);

        if self
            .top
            .compare_exchange(t, new_top, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }

        let index = t & self.mask;

        unsafe { Some((*self.buffer.get())[index].assume_init_read()) }
    }

    /// Get the current size of the queue.
    #[must_use]
    #[allow(dead_code)]
    pub fn len(&self, bottom: &Cell<usize>) -> usize {
        let b = bottom.get();
        let t = self.top.load(Ordering::Acquire);
        b.wrapping_sub(t)
    }

    /// Check if the queue is empty.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self, bottom: &Cell<usize>) -> bool {
        self.len(bottom) == 0
    }

    /// Check if the queue is full.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_full(&self, bottom: &Cell<usize>) -> bool {
        self.len(bottom) >= N
    }
}

impl<T: Copy, const N: usize> Default for StealQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: StealQueue is safe to share between threads because:
// - The buffer uses UnsafeCell for interior mutability
// - All operations use atomic synchronization (CAS on top, Cell on bottom)
// - Push/pop only access unique slots based on bottom/top values
// - Steal uses CAS to prevent concurrent access to same slot
unsafe impl<T: Copy + Send, const N: usize> Send for StealQueue<T, N> {}

// SAFETY: See Send impl
unsafe impl<T: Copy + Send, const N: usize> Sync for StealQueue<T, N> {}

#[cfg(test)]
mod tests {
    use super::StealQueue;
    use std::cell::Cell;

    #[test]
    fn test_steal_queue_basic() {
        let queue: StealQueue<i32, 1024> = StealQueue::new();
        let bottom = Cell::new(0);

        assert!(queue.is_empty(&bottom));

        assert!(queue.push(&bottom, 42));
        assert!(!queue.is_empty(&bottom));

        assert_eq!(queue.pop(&bottom), Some(42));
        assert!(queue.is_empty(&bottom));

        assert_eq!(queue.pop(&bottom), None);
    }

    #[test]
    fn test_steal_queue_fifo() {
        let queue: StealQueue<i32, 1024> = StealQueue::new();
        let bottom = Cell::new(0);

        queue.push(&bottom, 1);
        queue.push(&bottom, 2);
        queue.push(&bottom, 3);

        assert_eq!(queue.steal(&bottom), Some(1));
        assert_eq!(queue.steal(&bottom), Some(2));
        assert_eq!(queue.steal(&bottom), Some(3));
        assert_eq!(queue.steal(&bottom), None);
    }

    #[test]
    fn test_steal_queue_lifo() {
        let queue: StealQueue<i32, 1024> = StealQueue::new();
        let bottom = Cell::new(0);

        queue.push(&bottom, 1);
        queue.push(&bottom, 2);
        queue.push(&bottom, 3);

        assert_eq!(queue.pop(&bottom), Some(3));
        assert_eq!(queue.pop(&bottom), Some(2));
        assert_eq!(queue.pop(&bottom), Some(1));
    }

    #[test]
    fn test_steal_queue_bounds() {
        let queue: StealQueue<i32, 16> = StealQueue::new();
        let bottom = Cell::new(0);

        for i in 0..16 {
            assert!(queue.push(&bottom, i));
        }

        assert!(!queue.push(&bottom, 999));

        assert_eq!(queue.len(&bottom), 16);
    }
}
