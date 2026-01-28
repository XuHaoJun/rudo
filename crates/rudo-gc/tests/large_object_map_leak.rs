use rudo_gc::heap::{page_size, segment_manager};
use rudo_gc::Gc;
use std::sync::PoisonError;

#[derive(rudo_gc::Trace)]
#[allow(clippy::large_stack_arrays)]
struct LargeObject {
    data: [u8; 5000],
}

#[test]
fn test_large_object_map_cleanup() {
    let addr = std::thread::spawn(|| {
        let g = Gc::new(LargeObject { data: [0; 5000] });
        let ptr = Gc::as_ptr(&g) as usize;
        let header_addr = ptr & !(page_size() - 1);
        drop(g);
        header_addr
    })
    .join()
    .unwrap();

    let contains = segment_manager()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .large_object_map
        .contains_key(&addr);
    assert!(contains, "Large object should be in global map before GC");

    let mut allocations = Vec::new();
    for _ in 0..2500 {
        allocations.push(Gc::new([0u8; 4096]));
    }

    drop(allocations);

    rudo_gc::collect_full();

    let contains = segment_manager()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .large_object_map
        .contains_key(&addr);
    assert!(
        !contains,
        "Large object should be removed from global map after GC"
    );
}

#[test]
fn test_large_object_global_map_cleanup_on_thread_exit() {
    let page_addrs = std::thread::spawn(|| {
        let g = Gc::new(LargeObject { data: [0; 5000] });
        let ptr = Gc::as_ptr(&g) as usize;
        let header_addr = ptr & !(page_size() - 1);

        let pages_needed = 2;
        let mut addrs = Vec::new();
        for p in 0..pages_needed {
            addrs.push(header_addr + (p * page_size()));
        }
        drop(g);
        addrs
    })
    .join()
    .unwrap();

    let mut allocations = Vec::new();
    for _ in 0..2500 {
        allocations.push(Gc::new([0u8; 4096]));
    }

    drop(allocations);

    rudo_gc::collect_full();

    let manager = segment_manager()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    for addr in &page_addrs {
        let in_map = manager.large_object_map.contains_key(addr);
        let in_orphan = manager.orphan_pages.iter().any(|p| p.addr == *addr);
        assert!(
            !in_map && !in_orphan,
            "Addr {addr:x} should be removed from both map and orphan_pages after GC (in_map={in_map}, in_orphan={in_orphan})"
        );
    }
    drop(manager);
}
