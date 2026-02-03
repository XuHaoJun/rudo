//! Loom tests for dirty page list concurrent access.
//!
//! These tests verify the correctness of the double-check pattern
//! used in `add_to_dirty_pages` and the snapshot mechanism.
#![allow(
    clippy::cast_ptr_alignment,
    clippy::borrow_as_ptr,
    clippy::ptr_as_ptr,
    clippy::ref_as_ptr
)]

use std::alloc::{self, handle_alloc_error, Layout};
use std::ptr::NonNull;
use std::sync::atomic::AtomicU16;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicU8;

use rudo_gc::heap::{LocalHeap, PageHeader, BITMAP_SIZE};

fn create_test_page() -> NonNull<PageHeader> {
    let layout = Layout::new::<PageHeader>();
    let ptr = unsafe { alloc::alloc(layout).cast::<PageHeader>() };
    if ptr.is_null() {
        handle_alloc_error(layout);
    }
    unsafe {
        ptr.write(PageHeader {
            magic: 0xDC_BA_F0_0D,
            block_size: 4096,
            obj_count: 0,
            header_size: 64,
            generation: 0,
            flags: AtomicU8::new(0),
            owner_thread: 0,
            #[cfg(feature = "lazy-sweep")]
            dead_count: AtomicU16::new(0),
            #[cfg(not(feature = "lazy-sweep"))]
            _padding: [0u8; 2],
            mark_bitmap: [const { AtomicU64::new(0) }; BITMAP_SIZE],
            dirty_bitmap: [const { AtomicU64::new(0) }; BITMAP_SIZE],
            allocated_bitmap: [const { AtomicU64::new(0) }; BITMAP_SIZE],
            #[cfg(feature = "lazy-sweep")]
            free_list_head: AtomicU16::new(u16::MAX),
            #[cfg(not(feature = "lazy-sweep"))]
            free_list_head: u16::MAX,
        });
        NonNull::new_unchecked(ptr)
    }
}

fn destroy_test_page(header: NonNull<PageHeader>) {
    let layout = Layout::new::<PageHeader>();
    unsafe { alloc::dealloc(header.as_ptr().cast::<u8>(), layout) };
}

/// Test that concurrent `mark_page_dirty` calls don't cause duplicates.
/// Each thread marks the same page dirty - it should only appear once.
#[test]
#[ignore = "loom test - run with cargo test loom_dirty_page_concurrent_mark --release"]
fn test_concurrent_mark_same_page() {
    loom::model(|| {
        let mut heap = LocalHeap::new();
        let page1 = create_test_page();
        let page2 = create_test_page();

        let marker1 = loom::thread::spawn({
            let heap_ptr = (&heap as *const LocalHeap).cast_mut();
            move || unsafe { (*heap_ptr).add_to_dirty_pages(page1) }
        });

        let marker2 = loom::thread::spawn({
            let heap_ptr = (&heap as *const LocalHeap).cast_mut();
            move || unsafe { (*heap_ptr).add_to_dirty_pages(page1) }
        });

        marker1.join().unwrap();
        marker2.join().unwrap();

        let snapshot_size = heap.take_dirty_pages_snapshot();
        assert_eq!(snapshot_size, 1, "Page should appear exactly once");

        #[allow(clippy::needless_collect)]
        let snapshot: Vec<_> = heap.dirty_pages_iter().collect();
        assert_eq!(
            snapshot.len(),
            1,
            "Snapshot should contain exactly one page"
        );
        assert_eq!(snapshot[0], page1, "Snapshot should contain page1");

        destroy_test_page(page1);
        destroy_test_page(page2);
    });
}

/// Test that concurrent snapshot with marking maintains correctness.
#[test]
#[ignore = "loom test - run with cargo test loom_dirty_page_snapshot_and_mark --release"]
fn test_snapshot_and_concurrent_mark() {
    loom::model(|| {
        let mut heap = LocalHeap::new();
        let page1 = create_test_page();
        let page2 = create_test_page();
        let page3 = create_test_page();

        let marker_thread = loom::thread::spawn({
            let heap_ptr = (&heap as *const LocalHeap).cast_mut();
            move || unsafe {
                (*heap_ptr).add_to_dirty_pages(page1);
                (*heap_ptr).add_to_dirty_pages(page2);
                (*heap_ptr).add_to_dirty_pages(page3);
            }
        });

        let snapshot_thread = loom::thread::spawn({
            let heap_ptr = &mut heap as *mut LocalHeap;
            move || unsafe { (*heap_ptr).take_dirty_pages_snapshot() }
        });

        marker_thread.join().unwrap();
        let _snapshot_size = snapshot_thread.join().unwrap();

        let snapshot: Vec<_> = heap.dirty_pages_iter().collect();
        assert!(
            snapshot.len() <= 3,
            "Snapshot should not have more than 3 pages (was {})",
            snapshot.len()
        );

        destroy_test_page(page1);
        destroy_test_page(page2);
        destroy_test_page(page3);
    });
}

/// Test that marking different pages from multiple threads is safe.
#[test]
#[ignore = "loom test - run with cargo test loom_dirty_page_different_pages --release"]
fn test_concurrent_mark_different_pages() {
    loom::model(|| {
        let mut heap = LocalHeap::new();
        let pages: Vec<NonNull<PageHeader>> = (0..4).map(|_| create_test_page()).collect();

        let threads: Vec<_> = pages
            .iter()
            .map(|&page| {
                let heap_ptr = (&heap as *const LocalHeap).cast_mut();
                loom::thread::spawn(move || unsafe {
                    (*heap_ptr).add_to_dirty_pages(page);
                })
            })
            .collect();

        for thread in threads {
            thread.join().unwrap();
        }

        let snapshot_size = heap.take_dirty_pages_snapshot();
        assert_eq!(snapshot_size, 4, "All pages should appear exactly once");

        for &page in &pages {
            destroy_test_page(page);
        }
    });
}

/// Test the double-check pattern: flag check before and after lock acquisition.
#[test]
#[ignore = "loom test - run with cargo test loom_dirty_page_double_check --release"]
fn test_double_check_pattern() {
    loom::model(|| {
        let mut heap = LocalHeap::new();
        let page = create_test_page();

        let thread1 = loom::thread::spawn({
            let heap_ptr = (&heap as *const LocalHeap).cast_mut();
            move || unsafe { (*heap_ptr).add_to_dirty_pages(page) }
        });

        let thread2 = loom::thread::spawn({
            let heap_ptr = (&heap as *const LocalHeap).cast_mut();
            move || unsafe { (*heap_ptr).add_to_dirty_pages(page) }
        });

        thread1.join().unwrap();
        thread2.join().unwrap();

        let snapshot_size = heap.take_dirty_pages_snapshot();
        assert_eq!(
            snapshot_size, 1,
            "Page should appear exactly once despite concurrent adds"
        );

        assert_eq!(
            heap.dirty_pages_iter().count(),
            1,
            "Snapshot should contain exactly one page"
        );

        destroy_test_page(page);
    });
}
