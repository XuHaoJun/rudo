//! Miri tests for `HandleScope` unsafe operations.
//!
//! These tests are designed to be run with Miri to detect undefined behavior
//! in the unsafe handle operations.

use rudo_gc::handles::{AsyncHandleScope, EscapeableHandleScope, HandleScope};
use rudo_gc::heap::with_heap_and_tcb;
use rudo_gc::{Gc, Trace};

#[derive(Trace, Debug)]
struct MiriTestData {
    value: i32,
}

#[test]
fn miri_handle_get_returns_valid_reference() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(MiriTestData { value: 42 });
        let handle = scope.handle(&gc);

        let value = handle.get();
        assert_eq!(value.value, 42);
    });
}

#[test]
fn miri_handle_to_gc_preserves_data() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(MiriTestData { value: 99 });
        let handle = scope.handle(&gc);

        let gc2 = handle.to_gc();

        assert_eq!(gc2.value, 99);
    });
}

#[test]
fn miri_escapeable_handle_scope_escape() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let outer = HandleScope::new(tcb);

        let escaped = {
            let escape_scope = EscapeableHandleScope::new(tcb);
            let gc = Gc::new(MiriTestData { value: 123 });
            let inner = escape_scope.handle(&gc);

            escape_scope.escape(&outer, inner)
        };

        assert_eq!(escaped.value, 123);
    });
}

#[test]
fn miri_async_handle_across_await_points() {
    rudo_gc::test_util::reset();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let tcb = rudo_gc::heap::current_thread_control_block().expect("should have TCB");

        let scope = AsyncHandleScope::new(&tcb);
        let gc = Gc::new(MiriTestData { value: 777 });
        let handle = scope.handle(&gc);

        tokio::task::yield_now().await;

        let value = handle.get();
        assert_eq!(value.value, 777);

        drop(scope);
    });
}

#[test]
fn miri_async_handle_to_gc() {
    rudo_gc::test_util::reset();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let tcb = rudo_gc::heap::current_thread_control_block().expect("should have TCB");

        let scope = AsyncHandleScope::new(&tcb);
        let gc = Gc::new(MiriTestData { value: 555 });
        let handle = scope.handle(&gc);

        let gc2 = handle.to_gc();

        assert_eq!(gc2.value, 555);
    });
}

#[test]
fn miri_handle_copy_semantics() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(MiriTestData { value: 111 });
        let handle1 = scope.handle(&gc);

        let handle2 = handle1;
        let handle3 = handle1;

        assert_eq!(handle1.get().value, 111);
        assert_eq!(handle2.get().value, 111);
        assert_eq!(handle3.get().value, 111);
    });
}

#[test]
fn miri_async_handle_copy_semantics() {
    rudo_gc::test_util::reset();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let tcb = rudo_gc::heap::current_thread_control_block().expect("should have TCB");

        let scope = AsyncHandleScope::new(&tcb);
        let gc = Gc::new(MiriTestData { value: 222 });
        let handle1 = scope.handle(&gc);

        let handle2 = handle1;
        let handle3 = handle1;

        assert_eq!(handle1.get().value, 222);
        assert_eq!(handle2.get().value, 222);
        assert_eq!(handle3.get().value, 222);
    });
}

#[test]
fn miri_multiple_handles_same_scope() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);

        let gc1 = Gc::new(MiriTestData { value: 1 });
        let gc2 = Gc::new(MiriTestData { value: 2 });
        let gc3 = Gc::new(MiriTestData { value: 3 });

        let h1 = scope.handle(&gc1);
        let h2 = scope.handle(&gc2);
        let h3 = scope.handle(&gc3);

        assert_eq!(h1.get().value, 1);
        assert_eq!(h2.get().value, 2);
        assert_eq!(h3.get().value, 3);
    });
}

#[test]
fn miri_handle_deref_transparent() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(MiriTestData { value: 444 });
        let handle = scope.handle(&gc);

        assert_eq!(handle.get().value, 444);
    });
}

#[test]
fn miri_async_handle_guard_usage() {
    rudo_gc::test_util::reset();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let tcb = rudo_gc::heap::current_thread_control_block().expect("should have TCB");

        let scope = AsyncHandleScope::new(&tcb);
        let gc = Gc::new(MiriTestData { value: 333 });
        let handle = scope.handle(&gc);

        let result = scope.with_guard(|guard| guard.get(&handle).value);

        assert_eq!(result, 333);
    });
}
