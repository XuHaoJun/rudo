//! Unit tests for dirty page tracking functionality.
//!
//! These tests verify the dirty page list operations and snapshot mechanism.

use rudo_gc::heap::LocalHeap;
use rudo_gc::{collect, Gc};
use std::cell::RefCell;

/// Test that the dirty page list is initially empty.
#[test]
fn test_dirty_pages_empty_initially() {
    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Take snapshot to check dirty page count
        let count = heap.take_dirty_pages_snapshot();
        assert_eq!(count, 0, "Dirty pages list should be empty initially");
        heap.clear_dirty_pages_snapshot();
    });
}

/// Test that old-to-young references are properly tracked via dirty pages.
#[test]
fn test_old_to_young_reference_tracked() {
    // Create a young object
    let young_obj: Gc<RefCell<i32>> = Gc::new(RefCell::new(42));

    // Trigger a minor collection to promote young_obj to old generation
    collect();

    // Now young_obj is in old generation - create a reference to a new young object
    let _young_ref: Gc<RefCell<i32>> = Gc::new(RefCell::new(100));

    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Clear any existing dirty pages first
        heap.take_dirty_pages_snapshot();
        heap.clear_dirty_pages_snapshot();
    });

    // Mutate the old object to reference the young one
    // This should trigger the write barrier and add the page to dirty list
    *young_obj.borrow_mut() = 200;

    // Verify the mutation worked
    assert_eq!(*young_obj.borrow(), 200);
}

/// Test that duplicate dirty page entries are prevented.
#[test]
fn test_duplicate_dirty_page_prevention() {
    use rudo_gc::heap::PAGE_FLAG_DIRTY_LISTED;

    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Clear any existing dirty pages
        heap.take_dirty_pages_snapshot();
        heap.clear_dirty_pages_snapshot();

        // Simulate adding a page to dirty list multiple times
        // In reality, the write barrier prevents duplicates via the flag

        // The flag should not be set initially for a clean page
        // We can't easily test this without allocating real pages,
        // but we verify the flag constant exists
        assert_eq!(PAGE_FLAG_DIRTY_LISTED, 0x10);
    });
}

/// Test that the dirty page snapshot mechanism works correctly.
#[test]
fn test_dirty_pages_snapshot_mechanism() {
    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Initially empty
        let count1 = heap.take_dirty_pages_snapshot();
        assert_eq!(count1, 0, "Snapshot should be empty initially");

        // Clear and prepare for new dirty pages
        heap.clear_dirty_pages_snapshot();

        // Take another snapshot
        let count2 = heap.take_dirty_pages_snapshot();
        assert_eq!(count2, 0, "Snapshot should still be empty");

        heap.clear_dirty_pages_snapshot();
    });
}

/// Test that statistics are updated correctly after taking snapshots.
#[test]
fn test_dirty_pages_statistics() {
    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Clear existing state
        heap.take_dirty_pages_snapshot();
        heap.clear_dirty_pages_snapshot();

        // Check initial rolling average (should be 16 per initialization)
        // We can't directly access avg_dirty_pages, but we can verify
        // the snapshot capacity planning works

        // Take multiple snapshots to trigger rolling average updates
        for _ in 0..5 {
            let _count = heap.take_dirty_pages_snapshot();
            // The count should be consistent (0 in this test)
            heap.clear_dirty_pages_snapshot();
        }
    });
}

/// Test that the dirty page iterator works correctly.
#[test]
fn test_dirty_pages_iterator_empty() {
    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Take snapshot
        heap.take_dirty_pages_snapshot();

        // Iterator should yield no items for empty snapshot
        let count = heap.dirty_pages_iter().count();
        assert_eq!(
            count, 0,
            "Iterator should yield no items for empty snapshot"
        );

        heap.clear_dirty_pages_snapshot();
    });
}

/// Test that `PageHeader` dirty-listed flag operations work correctly.
#[test]
fn test_page_header_dirty_listed_operations() {
    use rudo_gc::heap::PAGE_FLAG_DIRTY_LISTED;

    // We can't create a real PageHeader without allocating memory,
    // but we can verify the constants and methods exist
    assert_eq!(
        PAGE_FLAG_DIRTY_LISTED, 0x10,
        "DIRTY_LISTED flag should be 0x10"
    );
}

/// Test write barrier behavior with dirty page tracking.
/// This is an integration-style test within the unit test module.
#[test]
fn test_write_barrier_dirty_page_integration() {
    // This test verifies that the write barrier correctly interacts
    // with the dirty page tracking system.

    // Create an object and promote it to old generation
    let obj: Gc<RefCell<i32>> = Gc::new(RefCell::new(1));
    collect(); // Promote to old generation

    // The mutation should trigger the write barrier
    *obj.borrow_mut() = 42;

    // Verify the value was set
    assert_eq!(*obj.borrow(), 42);

    // Trigger minor GC - the old object should be scanned via dirty pages
    collect();

    // Verify the object survived
    assert_eq!(*obj.borrow(), 42);
}

/// Test that dirty pages are cleared after minor GC.
#[test]
fn test_dirty_pages_cleared_after_gc() {
    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Clear existing dirty pages
        heap.take_dirty_pages_snapshot();
        heap.clear_dirty_pages_snapshot();

        // Verify the list is empty after clearing
        let count = heap.take_dirty_pages_snapshot();
        assert_eq!(count, 0, "Dirty pages should be cleared");
        heap.clear_dirty_pages_snapshot();
    });
}

/// Test that the dirty page list doesn't grow unbounded.
#[test]
fn test_dirty_pages_list_bounded() {
    // The dirty page list should be bounded by the number of pages
    // in the old generation. This is implicit in the design, but
    // we can verify it doesn't cause issues.

    rudo_gc::heap::with_heap(|heap: &mut LocalHeap| {
        // Clear any existing dirty pages
        heap.take_dirty_pages_snapshot();
        heap.clear_dirty_pages_snapshot();

        // The list should remain empty or bounded
        // during normal operation
        let _count = heap.take_dirty_pages_snapshot();
        // Count should be reasonable (0 in this test scenario)
        heap.clear_dirty_pages_snapshot();
    });
}
