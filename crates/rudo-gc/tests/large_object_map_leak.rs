use rudo_gc::heap::{segment_manager, PAGE_SIZE};
use rudo_gc::Gc;

/// Clear CPU callee-saved registers to prevent stale pointer values from being
/// treated as roots by the conservative GC.
///
/// # Safety
///
/// This function clears callee-saved registers (R12-R15 on `x86_64`).
/// It should only be called when those registers don't contain values
/// needed by the calling code.
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

        // CRITICAL: Clear callee-saved registers to prevent stale pointer values
        // from being treated as roots by the conservative GC.
        // Without this, the GC might find a stale pointer value in a register
        // and incorrectly keep the large object alive.
        unsafe {
            clear_registers();
        }

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

#[test]
fn test_large_object_global_map_cleanup_on_thread_exit() {
    #[derive(rudo_gc::Trace)]
    struct Big {
        data: [u8; 5000],
    }

    let page_addrs = std::thread::spawn(|| {
        let g = Gc::new(Big { data: [0; 5000] });
        let ptr = Gc::as_ptr(&g) as usize;
        let header_addr = ptr & !(PAGE_SIZE - 1);

        // Large object 5000 bytes + header (~128 bytes) = 5128 bytes.
        // Needs 2 pages.
        let total_size: usize = 5000 + 128; // Approximate
        let pages_needed = total_size.div_ceil(PAGE_SIZE);

        let mut addrs = Vec::new();
        for p in 0..pages_needed {
            addrs.push(header_addr + (p * PAGE_SIZE));
        }

        // Verify they are in the global map
        let manager = segment_manager().lock().unwrap();
        for addr in &addrs {
            assert!(
                manager.large_object_map.contains_key(addr),
                "Addr {addr:x} should be in global map before thread exit"
            );
        }
        drop(manager);
        addrs
    })
    .join()
    .unwrap();

    // Now the thread has exited, LocalHeap::drop should have run.
    // Verify all pages are removed from the global map.
    let manager = segment_manager().lock().unwrap();
    for addr in page_addrs {
        assert!(
            !manager.large_object_map.contains_key(&addr),
            "Addr {addr:x} should have been removed from global map after thread exit"
        );
    }
    drop(manager);
}
