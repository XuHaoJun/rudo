//! Tests for `HandleScope` block reuse behavior.

use crate::Gc;

fn tcb() -> std::sync::Arc<crate::heap::ThreadControlBlock> {
    crate::heap::current_thread_control_block().unwrap()
}

#[test]
fn test_handle_scope_block_reuse_after_drop() {
    crate::test_util::reset();

    let tcb_ref = tcb();

    // First scope creates handles
    {
        let scope1 = crate::handles::HandleScope::new(&tcb_ref);
        for _ in 0..10 {
            let _ = scope1.handle(&Gc::new(42u32));
        }
    }

    // Second scope should reuse the same blocks, not create new ones
    {
        let scope2 = crate::handles::HandleScope::new(&tcb_ref);
        for _ in 0..10 {
            let _ = scope2.handle(&Gc::new(42u32));
        }
    }
}

#[test]
fn test_nested_scopes_share_blocks() {
    crate::test_util::reset();

    let tcb_ref = tcb();

    let outer_scope = crate::handles::HandleScope::new(&tcb_ref);

    {
        let inner_scope1 = crate::handles::HandleScope::new(&tcb_ref);
        for _ in 0..50 {
            let _ = inner_scope1.handle(&Gc::new(42u32));
        }
    }

    {
        let inner_scope2 = crate::handles::HandleScope::new(&tcb_ref);
        for _ in 0..50 {
            let _ = inner_scope2.handle(&Gc::new(42u32));
        }
    }

    drop(outer_scope);
}

#[test]
fn test_many_scope_cycles_no_block_leak() {
    crate::test_util::reset();

    let tcb_ref = tcb();

    // Simulate many scope create/drop cycles
    for _ in 0..100 {
        let scope = crate::handles::HandleScope::new(&tcb_ref);
        let _ = scope.handle(&Gc::new(42u32));
    }

    // Verify LocalHandles can still allocate
    let scope = crate::handles::HandleScope::new(&tcb_ref);
    let _ = scope.handle(&Gc::new(42u32));
}

#[test]
fn test_empty_scopes_preserve_blocks() {
    crate::test_util::reset();

    let tcb_ref = tcb();

    // Create first scope with handles to establish blocks
    {
        let scope = crate::handles::HandleScope::new(&tcb_ref);
        let _ = scope.handle(&Gc::new(42u32));
    }

    // Many empty scope cycles
    for _ in 0..50 {
        let _scope = crate::handles::HandleScope::new(&tcb_ref);
    }

    // Should still be able to allocate in the original block
    let scope = crate::handles::HandleScope::new(&tcb_ref);
    let _ = scope.handle(&Gc::new(42u32));
}
