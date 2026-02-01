//! Integration tests for `HandleScope` GC root tracking.
//!
//! These tests verify that handles are correctly tracked as GC roots
//! and that objects referenced by handles survive collection.

use rudo_gc::handles::{AsyncHandleScope, EscapeableHandleScope, HandleScope};
use rudo_gc::heap::with_heap_and_tcb;
use rudo_gc::{Gc, Trace};

#[derive(Trace, Debug, PartialEq)]
struct GcRootTestData {
    value: i32,
}

#[test]
fn handles_survive_gc() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(GcRootTestData { value: 42 });
        let _handle = scope.handle(&gc);

        drop(scope);

        rudo_gc::collect();

        let gc2 = gc;
        assert_eq!(gc2.value, 42);
    });
}

#[test]
fn escaped_handle_survives_gc() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let outer = HandleScope::new(tcb);
        let gc_outer = Gc::new(GcRootTestData { value: 100 });
        let _h_outer = outer.handle(&gc_outer);

        {
            let escape_scope = EscapeableHandleScope::new(tcb);
            let gc = Gc::new(GcRootTestData { value: 200 });
            let inner = escape_scope.handle(&gc);
            let _escaped = escape_scope.escape(&outer, inner);

            drop(escape_scope);
            rudo_gc::collect();

            assert_eq!(gc.value, 200);
        }

        rudo_gc::collect();

        assert_eq!(gc_outer.value, 100);
    });
}

#[test]
fn multiple_handles_all_survive_gc() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);

        let gc1 = Gc::new(GcRootTestData { value: 1 });
        let gc2 = Gc::new(GcRootTestData { value: 2 });
        let gc3 = Gc::new(GcRootTestData { value: 3 });

        let _h1 = scope.handle(&gc1);
        let _h2 = scope.handle(&gc2);
        let _h3 = scope.handle(&gc3);

        drop(scope);

        rudo_gc::collect();

        assert_eq!(gc1.value, 1);
        assert_eq!(gc2.value, 2);
        assert_eq!(gc3.value, 3);
    });
}

#[test]
fn nested_scopes_handles_survive() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let outer_scope = HandleScope::new(tcb);
        let gc_outer = Gc::new(GcRootTestData { value: 0 });
        let _h_outer = outer_scope.handle(&gc_outer);

        {
            let inner_scope = HandleScope::new(tcb);
            let gc_inner = Gc::new(GcRootTestData { value: 1 });
            let _h_inner = inner_scope.handle(&gc_inner);

            drop(inner_scope);
            rudo_gc::collect();

            assert_eq!(gc_outer.value, 0);
        }

        rudo_gc::collect();

        assert_eq!(gc_outer.value, 0);
    });
}

#[test]
fn async_handles_survive_gc() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let tcb = rudo_gc::heap::current_thread_control_block().expect("should have TCB");

        let scope = AsyncHandleScope::new(&tcb);
        let gc = Gc::new(GcRootTestData { value: 777 });
        let _handle = scope.handle(&gc);

        rudo_gc::collect();

        assert_eq!(gc.value, 777);

        drop(scope);
    });
}

#[test]
fn async_handles_survive_multiple_gc_cycles() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let tcb = rudo_gc::heap::current_thread_control_block().expect("should have TCB");

        let scope = AsyncHandleScope::new(&tcb);
        let gc = Gc::new(GcRootTestData { value: 888 });
        let _handle = scope.handle(&gc);

        for _ in 0..3 {
            rudo_gc::collect();
            assert_eq!(gc.value, 888);
        }

        drop(scope);
    });
}

#[test]
fn spawn_with_gc_handles_survive_gc() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let _tcb = rudo_gc::heap::current_thread_control_block().expect("should have TCB");
        let gc = Gc::new(GcRootTestData { value: 999 });

        let result: std::result::Result<i32, _> = rudo_gc::spawn_with_gc!(gc => |h| {
            rudo_gc::collect();
            unsafe { h.get().value }
        })
        .await;

        assert_eq!(result.unwrap(), 999);
    });
}

#[test]
fn handle_to_gc_survives_scope_drop() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(GcRootTestData { value: 555 });
        let handle = scope.handle(&gc);

        let escaped_gc = handle.to_gc();

        drop(scope);

        rudo_gc::collect();

        assert_eq!(escaped_gc.value, 555);
    });
}

#[test]
fn handle_scope_level_tracking() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let outer = HandleScope::new(tcb);
        assert_eq!(outer.level(), 1);

        {
            let inner = HandleScope::new(tcb);
            assert_eq!(inner.level(), 2);
        }

        assert_eq!(outer.level(), 1);
    });
}

#[test]
fn maybe_handle_pattern() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);

        let gc = Gc::new(GcRootTestData { value: 333 });
        let handle = scope.handle(&gc);

        let maybe = rudo_gc::handles::MaybeHandle::from_handle(handle);

        assert!(!maybe.is_empty());

        let recovered = maybe.to_handle().unwrap();
        assert_eq!(recovered.get().value, 333);
    });
}

#[test]
fn empty_maybe_handle() {
    let empty = rudo_gc::handles::MaybeHandle::<i32>::empty();
    assert!(empty.is_empty());
    assert!(empty.to_handle().is_none());
}
