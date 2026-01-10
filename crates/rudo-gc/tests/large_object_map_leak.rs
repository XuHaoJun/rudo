use rudo_gc::heap::{segment_manager, PAGE_SIZE};
use rudo_gc::Gc;

#[test]
fn test_large_object_map_cleanup() {
    #[derive(rudo_gc::Trace)]
    struct Big {
        _data: [u8; 5000],
    }

    #[inline(never)]
    fn do_alloc() {
        let _g = Gc::new(Big { _data: [0; 5000] });
        let ptr = Gc::as_ptr(&_g) as usize;
        let addr = ptr & !(PAGE_SIZE - 1);

        // Verify it's in the global map
        let manager = segment_manager().lock().unwrap();
        assert!(
            manager.large_object_map.contains_key(&addr),
            "Should be in global map"
        );
    }

    std::thread::spawn(|| {
        do_alloc();

        #[inline(never)]
        fn clear_stack() {
            let mut x = [0u64; 1024];
            for i in 0..1024 {
                x[i] = 0;
            }
            std::hint::black_box(&mut x);
        }
        clear_stack();

        // Force a full collection to trigger sweep_large_objects
        rudo_gc::collect_full();

        // Verify it's removed from the global map
        let manager = segment_manager().lock().unwrap();
        assert!(
            manager.large_object_map.is_empty(),
            "Global large_object_map should be empty after sweep, but contains {} entries",
            manager.large_object_map.len()
        );
    })
    .join()
    .unwrap();
}
