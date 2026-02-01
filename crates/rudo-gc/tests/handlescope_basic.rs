//! Integration tests for `HandleScope` and `Handle`.

use rudo_gc::handles::{EscapeableHandleScope, HandleScope, MaybeHandle, SealedHandleScope};
use rudo_gc::heap::with_heap_and_tcb;
use rudo_gc::{Gc, Trace};

#[derive(Trace, Debug, PartialEq)]
struct TestData {
    value: i32,
}

#[test]
fn test_handlescope_creation() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        assert_eq!(scope.level(), 1);
    });
}

#[test]
fn test_handlescope_nested() {
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
fn test_handle_creation() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(TestData { value: 42 });
        let handle = scope.handle(&gc);

        assert_eq!(handle.get().value, 42);
        assert_eq!(handle.value, 42);
    });
}

#[test]
fn test_handle_deref() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(100i32);
        let handle = scope.handle(&gc);

        assert_eq!(*handle, 100);
    });
}

#[test]
fn test_handle_to_gc() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(TestData { value: 99 });
        let handle = scope.handle(&gc);

        let gc2 = handle.to_gc();
        assert_eq!(gc2.value, 99);
    });
}

#[test]
fn test_handle_copy_clone() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(55i32);
        let handle1 = scope.handle(&gc);
        let handle2 = handle1;
        let handle3 = handle1;

        assert_eq!(*handle1, 55);
        assert_eq!(*handle2, 55);
        assert_eq!(*handle3, 55);
    });
}

#[test]
fn test_multiple_handles_same_scope() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);

        let gc1 = Gc::new(1i32);
        let gc2 = Gc::new(2i32);
        let gc3 = Gc::new(3i32);

        let h1 = scope.handle(&gc1);
        let h2 = scope.handle(&gc2);
        let h3 = scope.handle(&gc3);

        assert_eq!(*h1, 1);
        assert_eq!(*h2, 2);
        assert_eq!(*h3, 3);
    });
}

#[test]
fn test_escapeable_handlescope_creation() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let outer = HandleScope::new(tcb);
        let _escape_scope = EscapeableHandleScope::new(tcb);
        drop(outer);
    });
}

#[test]
fn test_escapeable_handlescope_handle() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let _outer = HandleScope::new(tcb);
        let escape_scope = EscapeableHandleScope::new(tcb);

        let gc = Gc::new(77i32);
        let handle = escape_scope.handle(&gc);
        assert_eq!(*handle, 77);
    });
}

#[test]
fn test_escapeable_handlescope_escape() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let outer = HandleScope::new(tcb);

        let escaped_handle = {
            let escape_scope = EscapeableHandleScope::new(tcb);
            let gc = Gc::new(123i32);
            let inner_handle = escape_scope.handle(&gc);
            escape_scope.escape(&outer, inner_handle)
        };

        assert_eq!(*escaped_handle, 123);
    });
}

#[test]
#[should_panic(expected = "can only be called once")]
fn test_escapeable_handlescope_double_escape_panics() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let outer = HandleScope::new(tcb);
        let escape_scope = EscapeableHandleScope::new(tcb);

        let gc1 = Gc::new(1i32);
        let gc2 = Gc::new(2i32);

        let h1 = escape_scope.handle(&gc1);
        let h2 = escape_scope.handle(&gc2);

        let _ = escape_scope.escape(&outer, h1);
        let _ = escape_scope.escape(&outer, h2);
    });
}

#[test]
fn test_maybe_handle_empty() {
    let maybe: MaybeHandle<'_, i32> = MaybeHandle::empty();
    assert!(maybe.is_empty());
    assert!(maybe.to_handle().is_none());
}

#[test]
fn test_maybe_handle_from_handle() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(42i32);
        let handle = scope.handle(&gc);

        let maybe = MaybeHandle::from_handle(handle);
        assert!(!maybe.is_empty());

        let recovered = maybe.to_handle().unwrap();
        assert_eq!(*recovered, 42);
    });
}

#[test]
fn test_maybe_handle_copy() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(99i32);
        let handle = scope.handle(&gc);

        let maybe1 = MaybeHandle::from_handle(handle);
        let maybe2 = maybe1;

        assert!(!maybe1.is_empty());
        assert!(!maybe2.is_empty());
    });
}

#[cfg(debug_assertions)]
#[test]
fn test_sealed_handlescope_creation() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let _scope = HandleScope::new(tcb);
        let _sealed = SealedHandleScope::new(tcb);
    });
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "sealed scope")]
fn test_sealed_handlescope_prevents_allocation() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let _sealed = SealedHandleScope::new(tcb);

        let gc = Gc::new(42i32);
        let _ = scope.handle(&gc);
    });
}

#[test]
fn test_handle_display() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(42i32);
        let handle = scope.handle(&gc);

        let s = format!("{handle}");
        assert_eq!(s, "42");
    });
}

#[test]
fn test_handle_debug() {
    rudo_gc::test_util::reset();

    with_heap_and_tcb(|_, tcb| {
        let scope = HandleScope::new(tcb);
        let gc = Gc::new(42i32);
        let handle = scope.handle(&gc);

        let s = format!("{handle:?}");
        assert!(s.contains("Handle"));
        assert!(s.contains("42"));
    });
}
