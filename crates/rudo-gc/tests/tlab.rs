use rudo_gc::heap::{with_heap, PAGE_SIZE};
use rudo_gc::Gc;
use std::thread;

#[test]
fn test_tlab_thread_isolation() {
    let t1 = thread::spawn(|| {
        // Allocate a small object to trigger TLAB initialization/page allocation
        let _g1 = Gc::new(42i32);
        with_heap(|h| {
            assert!(h.pages.len() >= 1);
            // Return the first page address
            h.pages[0].as_ptr() as usize
        })
    });

    let t2 = thread::spawn(|| {
        // Allocate a small object
        let _g2 = Gc::new(43i32);
        with_heap(|h| {
            assert!(h.pages.len() >= 1);
            // Return the first page address
            h.pages[0].as_ptr() as usize
        })
    });

    let addr1 = t1.join().unwrap();
    let addr2 = t2.join().unwrap();

    assert_ne!(
        addr1, addr2,
        "Each thread should have its own TLAB and unique pages"
    );
}

#[test]
fn test_tlab_exhaustion_allocates_new_page() {
    // We want to exhaust a TLAB for a specific size class.
    // GcBox<i32> is ~36 bytes, which fits in the 64-byte size class.
    // PAGE_SIZE is 4096.
    // Header size for 64-byte blocks is 128 bytes (aligned).
    // (4096 - 128) / 64 = 62 objects per page.

    let count = 70; // More than one page worth of 64-byte objects

    thread::spawn(move || {
        let initial_pages = with_heap(|h| h.pages.len());
        assert_eq!(initial_pages, 0, "New thread should have 0 pages initially");

        let mut pointers = Vec::new();
        for i in 0..count {
            pointers.push(Gc::new(i as i32));
        }

        let final_pages = with_heap(|h| h.pages.len());
        assert!(
            final_pages >= 2,
            "TLAB exhaustion should have forced at least one new page allocation (total >= 2), got {}",
            final_pages
        );

        // Verify they are not all in the same page
        let p1 = Gc::as_ptr(&pointers[0]) as usize;
        let p_last = Gc::as_ptr(&pointers[count - 1]) as usize;
        assert_ne!(
            p1 & !(PAGE_SIZE - 1),
            p_last & !(PAGE_SIZE - 1),
            "First and last objects should be in different pages"
        );
    }).join().unwrap();
}

#[test]
fn test_tlab_bump_pointer_contiguity() {
    // Sequential allocations in the same TLAB should be contiguous (within the same size class)
    thread::spawn(|| {
        let g1 = Gc::new(1i32);
        let g2 = Gc::new(2i32);
        let g3 = Gc::new(3i32);

        let p1 = Gc::as_ptr(&g1) as usize;
        let p2 = Gc::as_ptr(&g2) as usize;
        let p3 = Gc::as_ptr(&g3) as usize;

        // All should be in the same page
        let page = p1 & !(PAGE_SIZE - 1);
        assert_eq!(p2 & !(PAGE_SIZE - 1), page);
        assert_eq!(p3 & !(PAGE_SIZE - 1), page);

        // Distance between them should be constant (the size class)
        let diff1 = p2 - p1;
        let diff2 = p3 - p2;

        assert_eq!(
            diff1, diff2,
            "Bump pointer should allocate with constant stride"
        );
        // For i32, it's 64 bytes as calculated before.
        assert_eq!(diff1, 64, "Expected 64-byte size class for GcBox<i32>");
    })
    .join()
    .unwrap();
}

#[test]
fn test_mixed_size_class_tlabs() {
    // Different size classes should use different TLABs (and different pages)
    thread::spawn(|| {
        use rudo_gc::{Trace, Visitor};

        #[derive(Trace)]
        struct Large {
            _data: [u8; 100],
        }

        let g_small = Gc::new(()); // 32 bytes -> 32 size class
        let g_large = Gc::new(Large { _data: [0; 100] }); // 32 + 100 = 132 bytes -> 256 size class

        let p_small = Gc::as_ptr(&g_small) as usize;
        let p_large = Gc::as_ptr(&g_large) as usize;

        assert_ne!(
            p_small & !(PAGE_SIZE - 1),
            p_large & !(PAGE_SIZE - 1),
            "Different size classes must be in different pages"
        );

        with_heap(|h| {
            assert!(
                h.pages.len() >= 2,
                "Should have allocated at least 2 pages for 2 different size classes"
            );
        });
    })
    .join()
    .unwrap();
}

#[test]
fn test_free_slot_reuse() {
    // We want to verify that slots reclaimed by GC are reused for new allocations
    // instead of always allocating new pages.
    let count = 100;

    let initial_pages = thread::spawn(move || {
        let mut pointers = Vec::new();
        for i in 0..count {
            pointers.push(Gc::new(i as i32));
        }
        with_heap(|h| h.pages.len())
    })
    .join()
    .unwrap();

    assert!(
        initial_pages >= 2,
        "Should have at least 2 pages for 100 objects of 64 bytes"
    );

    thread::spawn(move || {
        // Clear stack to remove stale Gc pointers
        #[inline(never)]
        fn clear_stack() {
            let mut x = [0u64; 1024];
            std::hint::black_box(&mut x);
        }
        clear_stack();

        // Force collection
        rudo_gc::collect_full();

        // Allocate more objects
        let mut pointers = Vec::new();
        for i in 0..count {
            pointers.push(Gc::new(i as i32));
        }

        let final_pages = with_heap(|h| h.pages.len());

        assert!(
            final_pages <= initial_pages + 1,
            "Page count increased from {} to {} indicating no reuse of free slots",
            initial_pages,
            final_pages
        );
    })
    .join()
    .unwrap();
}
