//! Unit tests for local handle storage.

#[cfg(test)]
mod tests {
    use crate::handles::local_handles::{
        HandleBlock, HandleScopeData, HandleSlot, LocalHandles, HANDLE_BLOCK_SIZE,
    };
    use crate::ptr::GcBox;

    #[test]
    fn test_handle_slot_null() {
        let slot = HandleSlot::null();
        assert!(slot.is_null());
        assert!(slot.as_ptr().is_null());
    }

    #[test]
    fn test_handle_slot_new() {
        let fake_ptr = 0x1000 as *const GcBox<()>;
        let slot = HandleSlot::new(fake_ptr);
        assert!(!slot.is_null());
        assert_eq!(slot.as_ptr(), fake_ptr);
    }

    #[test]
    fn test_handle_slot_set() {
        let mut slot = HandleSlot::null();
        assert!(slot.is_null());

        let fake_ptr = 0x2000 as *const GcBox<()>;
        slot.set(fake_ptr);
        assert!(!slot.is_null());
        assert_eq!(slot.as_ptr(), fake_ptr);
    }

    #[test]
    fn test_handle_block_new() {
        let block = HandleBlock::new();
        assert!(block.next().is_none());
    }

    #[test]
    fn test_handle_block_slots_ptr() {
        let mut block = HandleBlock::new();
        let start = block.slots_ptr();
        let end = block.slots_end();

        let diff = (end as usize) - (start as usize);
        let expected = HANDLE_BLOCK_SIZE * std::mem::size_of::<HandleSlot>();
        assert_eq!(diff, expected);
    }

    #[test]
    fn test_handle_block_set_next() {
        use std::ptr::NonNull;

        let mut block1 = HandleBlock::new();
        let block2 = HandleBlock::new();
        let block2_ptr = NonNull::from(Box::leak(block2));

        assert!(block1.next().is_none());
        block1.set_next(Some(block2_ptr));
        assert!(block1.next().is_some());
        assert_eq!(block1.next().unwrap(), block2_ptr);

        unsafe {
            let _ = Box::from_raw(block2_ptr.as_ptr());
        }
    }

    #[test]
    fn test_handle_scope_data_new() {
        let data = HandleScopeData::new();
        assert!(data.next.is_null());
        assert!(data.limit.is_null());
        assert_eq!(data.level, 0);
        assert!(!data.is_active());
    }

    #[test]
    fn test_handle_scope_data_is_active() {
        let mut data = HandleScopeData::new();
        assert!(!data.is_active());

        data.level = 1;
        assert!(data.is_active());

        data.level = 5;
        assert!(data.is_active());
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_handle_scope_data_is_sealed() {
        let mut data = HandleScopeData::new();
        data.level = 2;
        data.sealed_level = 0;
        assert!(!data.is_sealed());

        data.sealed_level = 2;
        assert!(data.is_sealed());

        data.sealed_level = 3;
        assert!(data.is_sealed());

        data.level = 4;
        assert!(!data.is_sealed());
    }

    #[test]
    fn test_local_handles_new() {
        let handles = LocalHandles::new();
        assert!(!handles.scope_data().is_active());
    }

    #[test]
    fn test_local_handles_add_block() {
        let mut handles = LocalHandles::new();
        let (start, end) = handles.add_block();

        assert!(!start.is_null());
        assert!(!end.is_null());
        assert!(start < end);

        let diff = (end as usize) - (start as usize);
        let expected = HANDLE_BLOCK_SIZE * std::mem::size_of::<HandleSlot>();
        assert_eq!(diff, expected);
    }

    #[test]
    fn test_local_handles_allocate() {
        let mut handles = LocalHandles::new();

        let slot1 = handles.allocate();
        assert!(!slot1.is_null());

        let slot2 = handles.allocate();
        assert!(!slot2.is_null());
        assert_ne!(slot1, slot2);

        let diff = (slot2 as usize) - (slot1 as usize);
        assert_eq!(diff, std::mem::size_of::<HandleSlot>());
    }

    #[test]
    fn test_local_handles_allocate_multiple_blocks() {
        let mut handles = LocalHandles::new();

        for _ in 0..(HANDLE_BLOCK_SIZE + 10) {
            let slot = handles.allocate();
            assert!(!slot.is_null());
        }
    }

    #[test]
    fn test_local_handles_iterate_empty() {
        let handles = LocalHandles::new();
        let mut count = 0;
        handles.iterate(|_| count += 1);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_local_handles_iterate() {
        let mut handles = LocalHandles::new();

        let fake_ptr1 = 0x1000 as *const GcBox<()>;
        let fake_ptr2 = 0x2000 as *const GcBox<()>;
        let fake_ptr3 = 0x3000 as *const GcBox<()>;

        let slot1 = handles.allocate();
        unsafe { (*slot1).set(fake_ptr1) };

        let slot2 = handles.allocate();
        unsafe { (*slot2).set(fake_ptr2) };

        let slot3 = handles.allocate();
        unsafe { (*slot3).set(fake_ptr3) };

        let mut visited = Vec::new();
        handles.iterate(|ptr| visited.push(ptr));

        assert_eq!(visited.len(), 3);
        assert!(visited.contains(&fake_ptr1));
        assert!(visited.contains(&fake_ptr2));
        assert!(visited.contains(&fake_ptr3));
    }

    #[test]
    fn test_local_handles_iterate_skips_null() {
        let mut handles = LocalHandles::new();

        let fake_ptr = 0x1000 as *const GcBox<()>;

        let slot1 = handles.allocate();
        unsafe { (*slot1).set(fake_ptr) };

        let _slot2 = handles.allocate();

        let slot3 = handles.allocate();
        unsafe { (*slot3).set(fake_ptr) };

        let mut count = 0;
        handles.iterate(|_| count += 1);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_local_handles_scope_data_mut() {
        let mut handles = LocalHandles::new();

        {
            let data = handles.scope_data_mut();
            data.level = 5;
        }

        assert_eq!(handles.scope_data().level, 5);
    }
}
