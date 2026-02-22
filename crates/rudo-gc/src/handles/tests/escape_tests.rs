use crate::Gc;

fn tcb() -> std::sync::Arc<crate::heap::ThreadControlBlock> {
    crate::heap::current_thread_control_block().unwrap()
}

#[test]
fn test_gc_refcount_basics() {
    crate::test_util::reset();

    let gc = Gc::new(42u32);
    assert_eq!(Gc::ref_count(&gc).get(), 1);

    let gc2 = gc.clone();
    assert_eq!(Gc::ref_count(&gc).get(), 2);
    assert_eq!(Gc::ref_count(&gc2).get(), 2);

    drop(gc);
    assert_eq!(Gc::ref_count(&gc2).get(), 1);

    drop(gc2);
}

#[test]
fn test_gc_from_raw_does_not_increment_refcount() {
    crate::test_util::reset();

    let gc = Gc::new(42u32);
    assert_eq!(Gc::ref_count(&gc).get(), 1);

    let ptr = Gc::internal_ptr(&gc);
    let gc2 = unsafe { crate::test_util::from_raw::<u32>(ptr) };

    assert_eq!(Gc::ref_count(&gc).get(), 1, "from_raw does not increment");
    assert_eq!(
        Gc::ref_count(&gc2).get(),
        1,
        "from_raw creates independent Gc"
    );

    drop(gc);
    // gc2 points to freed memory (from_raw did not inc_ref); ref_count would panic per docs.
    drop(gc2);
}

#[test]
fn test_handle_to_gc_keeps_original_alive() {
    crate::test_util::reset();

    let gc = Gc::new(42u32);
    assert_eq!(Gc::ref_count(&gc).get(), 1);

    let tcb_ref = tcb();
    let scope = crate::handles::HandleScope::new(&tcb_ref);
    let handle = scope.handle(&gc);

    let escaped = handle.to_gc();
    assert_eq!(Gc::ref_count(&gc).get(), 2);

    drop(gc);
    assert_eq!(Gc::ref_count(&escaped).get(), 1);

    drop(escaped);
}

#[test]
fn test_async_handle_to_gc_basic() {
    crate::test_util::reset();

    let scope = crate::handles::AsyncHandleScope::new(&tcb());
    let gc = Gc::new(42u32);
    let handle = scope.handle(&gc);

    let gc1 = handle.to_gc();
    assert_eq!(Gc::ref_count(&gc1).get(), 1);

    drop(gc1);
    drop(scope);
}

#[test]
fn test_handle_to_gc_escape_pattern() {
    crate::test_util::reset();

    let gc = Gc::new(42u32);
    let tcb_ref = tcb();
    let _outer_scope = crate::handles::HandleScope::new(&tcb_ref);

    {
        let inner_scope = crate::handles::HandleScope::new(&tcb_ref);
        let handle = inner_scope.handle(&gc);
        let escaped = handle.to_gc();

        drop(inner_scope);
        assert!(Gc::try_deref(&escaped).is_some());

        drop(escaped);
    }

    assert!(Gc::try_deref(&gc).is_some());
    drop(gc);
}
