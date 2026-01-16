#[cfg(test)]
mod tests {
    use crate::heap::{page_mask, HEAP_HINT_ADDRESS};
    use std::hint::black_box;

    #[test]
    fn test_blacklisting_conflict() {
        struct BigStruct {
            _data: [u8; 4096],
        }

        //    let ptr = crate::heap::with_heap(crate::heap::LocalHeap::alloc::<i32>);

        // 2. Artificially plant a "bomb" on the stack.
        // We use the HINT address, which we know the allocator will try first.
        let bomb = HEAP_HINT_ADDRESS;

        // Ensure the compiler doesn't optimize away our bomb
        black_box(&bomb);

        // 3. Allocate something large enough to trigger a new page (or large object)
        // A large object guarantees a fresh mmap call.
        // We allocate 4KB + wrapper overhead essentially.
        // Smallest large object is > 2KB.
        // Let's allocate 4096 bytes.
        let ptr = crate::heap::with_heap(crate::heap::LocalHeap::alloc::<BigStruct>);

        // 4. Verification

        // The pointer we got should NOT be the bomb address.
        // Because the bomb was on the stack, the allocator should have:
        // a. Mmapped HEAP_HINT_ADDRESS
        // b. Scanned stack, found bomb
        // c. Quarantined it
        // d. Mmapped something else
        let addr = ptr.as_ptr() as usize;
        let page_addr = addr & page_mask();

        println!("Bomb: {bomb:#x}, Alloc: {addr:#x}, Page: {page_addr:#x}");

        // Note: For large objects, the pointer returned is NOT the page start, but offset by header.
        // But the conflict check is against [page_start, page_start + total_size).
        // If bomb == HEAP_HINT_ADDRESS, and HEAP_HINT_ADDRESS is usually page-aligned.
        // The allocator tries to map at HEAP_HINT_ADDRESS.

        // If the OS respected the hint, it mapped at HEAP_HINT_ADDRESS.
        // The conflict check saw 'bomb' (HEAP_HINT_ADDRESS) which falls in [HEAP_HINT_ADDRESS, ...).
        // STRICTLY SPEAKING: The conflict check logic:
        // if ptr >= start && ptr < end { found = true }
        // Our 'bomb' is exactly start. So it should trigger.

        assert_ne!(
            page_addr, bomb,
            "Allocator should have avoided the bomb on stack!"
        );

        // Also verify the heap tracked this quarantine
        // GlobalHeap.quarantined is private but we can infer it worked if address is different
        // AND we know sys_alloc tries hint first.
        // On Linux/Unix, mmap respects hint if available.
        // NOTE: If HEAP_HINT_ADDRESS was already taken by something else in the process,
        // we wouldn't have gotten it anyway, so the test might pass "by accident".
        // But this is the best we can do without invasive inspection.

        // To be extra sure, we can check if the bomb is strictly avoided
    }
}
