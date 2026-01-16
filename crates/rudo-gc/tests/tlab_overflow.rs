use rudo_gc::heap::{page_size, Tlab};

#[test]
fn test_tlab_overflow_prevention() {
    let mut tlab = Tlab::new();
    // Allocate a page-sized buffer for testing
    let layout = std::alloc::Layout::from_size_align(page_size(), page_size()).unwrap();
    let page = unsafe { std::alloc::alloc(layout) };

    let h_size = 128;
    let block_size = 48; // Non-power-of-two to create a remainder

    // (4096 - 128) / 48 = 82 objects
    // 82 * 48 = 3936
    // 128 + 3936 = 4064.
    // 4096 - 4064 = 32 bytes remaining.

    tlab.bump_ptr = unsafe { page.add(h_size) };
    // Set bump_end correctly as the new code should do
    let expected_bump_end = unsafe { page.add(h_size + 82 * block_size) };
    tlab.bump_end = expected_bump_end;

    // Allocate 82 objects
    for _ in 0..82 {
        assert!(tlab.alloc(block_size).is_some());
    }

    // Now tlab.bump_ptr should be exactly expected_bump_end
    assert_eq!(tlab.bump_ptr as usize, expected_bump_end as usize);

    // The 83rd allocation should fail because 32 < 48
    assert!(
        tlab.alloc(block_size).is_none(),
        "TLAB should not allow overflow"
    );

    // Verify even with a small remaining space (e.g. 1 byte) it fails
    tlab.bump_ptr = unsafe { expected_bump_end.sub(1) };
    assert!(
        tlab.alloc(block_size).is_none(),
        "TLAB should not allow overflow even with partial space"
    );

    unsafe { std::alloc::dealloc(page, layout) };
}

#[test]
fn test_tlab_init_uses_correct_bump_end() {
    // This test verifies that LocalHeap::alloc_slow (via Gc::new) sets bump_end correctly.
    // We'll use a thread to get a fresh LocalHeap.
    std::thread::spawn(|| {
        use rudo_gc::heap::with_heap;
        use rudo_gc::Gc;

        // Allocate something to trigger TLAB initialization.
        // GcBox<i32> is ~36 bytes, which fits in 64-byte size class.
        let _g = Gc::new(42i32);

        with_heap(|h| {
            // Find the 64-byte TLAB
            // Since we can't easily access private fields, we'll check the one that's initialized.
            // Actually, we can't easily find WHICH Tlab it is without match.
            // Let's just check all of them.

            // This is a bit hacky but works for verification.
            let tlabs = [
                &h.tlab_16,
                &h.tlab_32,
                &h.tlab_64,
                &h.tlab_128,
                &h.tlab_256,
                &h.tlab_512,
                &h.tlab_1024,
                &h.tlab_2048,
            ];

            let mut found = false;
            for tlab in tlabs {
                if let Some(page) = tlab.current_page {
                    found = true;
                    let page_start = page.as_ptr() as usize;
                    let bump_end = tlab.bump_end as usize;

                    // Since all current size classes are powers of two, they should all
                    // end exactly at the page boundary.
                    // But if we ever had a non-power-of-two size class, this would be different.
                    // For now, we can at least verify it's NOT past the page boundary.
                    assert!(bump_end <= page_start + page_size());

                    // And specifically for power-of-two size classes, it SHOULD be the page boundary.
                    assert_eq!(bump_end, page_start + page_size());
                }
            }
            assert!(found);
        });
    })
    .join()
    .unwrap();
}
