use rudo_gc::heap::{page_size, segment_manager};
use rudo_gc::Gc;

#[inline(never)]
unsafe fn clear_registers() {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    unsafe {
        std::arch::asm!(
            "xor r12, r12",
            "xor r13, r13",
            "xor r14, r14",
            "xor r15, r15",
            out("r12") _,
            out("r13") _,
            out("r14") _,
            out("r15") _,
        );
    }
    #[cfg(any(not(target_arch = "x86_64"), miri))]
    std::hint::black_box(());
}

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
        ptr & !(page_size() - 1)
    })
    .join()
    .unwrap();

    let contains = segment_manager()
        .lock()
        .unwrap()
        .large_object_map
        .contains_key(&addr);
    assert!(contains);

    unsafe {
        clear_registers();
    }

    let mut allocations = Vec::new();
    for _ in 0..2500 {
        allocations.push(Gc::new([0u8; 4096]));
    }

    drop(allocations);

    rudo_gc::collect_full();

    let contains = segment_manager()
        .lock()
        .unwrap()
        .large_object_map
        .contains_key(&addr);
    assert!(!contains);
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
        addrs
    })
    .join()
    .unwrap();

    let manager = segment_manager().lock().unwrap();
    for addr in &page_addrs {
        assert!(
            manager.large_object_map.contains_key(addr),
            "Addr {addr:x} should still be in map"
        );
    }
    drop(manager);

    unsafe {
        clear_registers();
    }

    let mut allocations = Vec::new();
    for _ in 0..2500 {
        allocations.push(Gc::new([0u8; 4096]));
    }

    drop(allocations);

    rudo_gc::collect_full();

    let manager = segment_manager().lock().unwrap();
    for addr in &page_addrs {
        assert!(
            !manager.large_object_map.contains_key(addr),
            "Addr {addr:x} should be removed"
        );
    }
    drop(manager);
}
