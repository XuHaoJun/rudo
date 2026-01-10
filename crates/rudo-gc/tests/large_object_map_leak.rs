use rudo_gc::heap::{segment_manager, PAGE_SIZE};
use rudo_gc::Gc;

#[test]
fn test_large_object_map_cleanup() {
    #[derive(rudo_gc::Trace)]
    struct Big {
        data: [u8; 5000],
    }

    #[inline(never)]
    fn clear_stack() {
        let mut x = [0u64; 1024];
        x.fill(0);
        std::hint::black_box(&mut x);
    }

    #[inline(never)]
    fn do_alloc() {
        let g = Gc::new(Big { data: [0; 5000] });
        let ptr = Gc::as_ptr(&g) as usize;
        let addr = ptr & !(PAGE_SIZE - 1);

        // Verify it's in the global map
        let contains = segment_manager()
            .lock()
            .unwrap()
            .large_object_map
            .contains_key(&addr);
        assert!(contains, "Should be in global map");
    }

    std::thread::spawn(move || {
        do_alloc();

        clear_stack();

        // Force a full collection to trigger sweep_large_objects
        rudo_gc::collect_full();

        // Verify it's removed from the global map
        let (is_empty, len) = {
            let manager = segment_manager().lock().unwrap();
            (
                manager.large_object_map.is_empty(),
                manager.large_object_map.len(),
            )
        };
        assert!(
            is_empty,
            "Global large_object_map should be empty after sweep, but contains {len} entries"
        );
    })
    .join()
    .unwrap();
}
