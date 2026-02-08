//! Integration tests for `AsyncHandleScope` and async handle support.

use rudo_gc::handles::AsyncHandleScope;
use rudo_gc::heap::{current_thread_control_block, with_heap_and_tcb_arc};
use rudo_gc::{Gc, Trace};

#[derive(Trace, Debug, PartialEq)]
struct AsyncTestData {
    value: i32,
}

#[test]
fn test_async_handlescope_creation() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);
        assert!(scope.id() > 0);
    });
}

#[test]
fn test_async_handlescope_handle() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);
        let gc = Gc::new(AsyncTestData { value: 42 });
        let handle = scope.handle(&gc);

        assert_eq!(handle.get().value, 42);
    });
}

#[test]
fn test_async_handle_get() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);
        let gc = Gc::new(100i32);
        let handle = scope.handle(&gc);

        assert_eq!(*handle.get(), 100);
    });
}

#[test]
fn test_async_handle_to_gc() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);
        let gc = Gc::new(AsyncTestData { value: 99 });
        let handle = scope.handle(&gc);

        let gc2 = handle.to_gc();
        assert_eq!(gc2.value, 99);
    });
}

#[test]
fn test_async_handle_copy_clone() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);
        let gc = Gc::new(55i32);
        let handle1 = scope.handle(&gc);
        let handle2 = handle1;
        let handle3 = handle1;

        assert_eq!(*handle1.get(), 55);
        assert_eq!(*handle2.get(), 55);
        assert_eq!(*handle3.get(), 55);
    });
}

#[test]
fn test_async_handle_scope_with_guard() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);
        let gc = Gc::new(AsyncTestData { value: 888 });
        let handle = scope.handle(&gc);

        let result = scope.with_guard(|guard| {
            assert_eq!(guard.get(&handle).value, 888);
            "success"
        });

        assert_eq!(result, "success");
    });
}

#[test]
fn test_multiple_async_handles() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);

        let gc1 = Gc::new(1i32);
        let gc2 = Gc::new(2i32);
        let gc3 = Gc::new(3i32);

        let h1 = scope.handle(&gc1);
        let h2 = scope.handle(&gc2);
        let h3 = scope.handle(&gc3);

        assert_eq!(*h1.get(), 1);
        assert_eq!(*h2.get(), 2);
        assert_eq!(*h3.get(), 3);
    });
}

#[test]
fn test_async_handle_scope_iterate() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);

        let gc1 = Gc::new(AsyncTestData { value: 10 });
        let gc2 = Gc::new(AsyncTestData { value: 20 });

        let _h1 = scope.handle(&gc1);
        let _h2 = scope.handle(&gc2);

        let mut count = 0;
        scope.iterate(|_ptr| {
            count += 1;
        });

        assert_eq!(count, 2);
    });
}

#[test]
fn test_async_handle_scope_id_uniqueness() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb_arc(|_, tcb| {
        let scope1 = AsyncHandleScope::new(tcb);
        let scope2 = AsyncHandleScope::new(tcb);
        let scope3 = AsyncHandleScope::new(tcb);

        assert_ne!(scope1.id(), scope2.id());
        assert_ne!(scope2.id(), scope3.id());
        assert_ne!(scope1.id(), scope3.id());
    });
}

#[test]
fn test_spawn_with_gc_macro_single_handle() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let _tcb = current_thread_control_block().expect("should have TCB");
        let gc = Gc::new(AsyncTestData { value: 42 });

        let result: std::result::Result<String, _> = rudo_gc::spawn_with_gc!(gc => |h| {
            assert_eq!(h.get().value, 42);
            "got_value".to_string()
        })
        .await;

        assert_eq!(result.unwrap(), "got_value");
    });
}

#[test]
fn test_spawn_with_gc_macro_multiple_handles() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let _tcb = current_thread_control_block().expect("should have TCB");
        let gc1 = Gc::new(AsyncTestData { value: 100 });
        let gc2 = Gc::new(AsyncTestData { value: 200 });

        let result: std::result::Result<i32, _> = rudo_gc::spawn_with_gc!(gc1, gc2 => |h1, h2| {
            assert_eq!(h1.get().value, 100);
            assert_eq!(h2.get().value, 200);
            h1.get().value + h2.get().value
        })
        .await;

        assert_eq!(result.unwrap(), 300);
    });
}

#[test]
fn test_async_handles_across_await_points() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let _tcb = current_thread_control_block().expect("should have TCB");
        let gc = Gc::new(AsyncTestData { value: 777 });

        let result: std::result::Result<i32, _> = rudo_gc::spawn_with_gc!(gc => |handle| {
            let first = handle.get().value;
            tokio::task::yield_now().await;
            let second = handle.get().value;
            first + second
        })
        .await;

        assert_eq!(result.unwrap(), 1554);
    });
}

#[test]
fn test_spawn_with_gc_handle_copy_across_await() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let _tcb = current_thread_control_block().expect("should have TCB");
        let gc = Gc::new(AsyncTestData { value: 50 });

        let result: std::result::Result<i32, _> = rudo_gc::spawn_with_gc!(gc => |h| {
            let handle_copy = h;
            tokio::task::yield_now().await;
            handle_copy.get().value * 2
        })
        .await;

        assert_eq!(result.unwrap(), 100);
    });
}

#[test]
fn test_async_handle_scope_drop_unregisters() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let tcb = current_thread_control_block().expect("should have TCB");

        {
            let scope = AsyncHandleScope::new(&tcb);
            let gc = Gc::new(AsyncTestData { value: 123 });
            let _handle = scope.handle(&gc);
            assert!(scope.id() > 0);
        }
    });
}

#[test]
fn test_spawn_with_gc_nested_async_operations() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let _tcb = current_thread_control_block().expect("should have TCB");
        let gc1 = Gc::new(AsyncTestData { value: 5 });
        let gc2 = Gc::new(AsyncTestData { value: 10 });

        let outer: tokio::task::JoinHandle<i32> = rudo_gc::spawn_with_gc!(gc1, gc2 => |h1, h2| {
            tokio::task::yield_now().await;
            let val1 = h1.get().value;
            let val2 = h2.get().value;
            val1 + val2
        });

        let result = outer.await;
        assert_eq!(result.unwrap(), 15);
    });
}

#[test]
fn test_spawn_with_gc_macro_complex_expression() {
    #[derive(Trace, Debug, PartialEq)]
    struct Container {
        gc: Gc<AsyncTestData>,
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        rudo_gc::test_util::reset();

        let _tcb = current_thread_control_block().expect("should have TCB");
        let container = Container {
            gc: Gc::new(AsyncTestData { value: 123 }),
        };

        let result: std::result::Result<i32, _> = rudo_gc::spawn_with_gc!(container.gc => |h| {
            h.get().value
        })
        .await;

        assert_eq!(result.unwrap(), 123);
    });
}
