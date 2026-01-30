#![allow(clippy::doc_markdown)]
#![allow(clippy::let_unit_value)]
#![allow(clippy::ignored_unit_patterns)]

//! Integration tests for tokio proc-macros: #[gc_main]

use rudo_gc::tokio::gc_main;
use rudo_gc::tokio::GcTokioExt;
use rudo_gc::Gc;
use std::sync::atomic::{AtomicUsize, Ordering};

static MAIN_CALLED: AtomicUsize = AtomicUsize::new(0);

#[gc_main]
async fn test_main_macro() {
    MAIN_CALLED.fetch_add(1, Ordering::SeqCst);

    let value = Gc::new(42);
    let _guard = value.root_guard();

    assert_eq!(*value, 42);

    assert_eq!(MAIN_CALLED.load(Ordering::SeqCst), 1);
}

#[gc_main(flavor = "current_thread")]
async fn test_main_macro_current_thread() {
    let value = Gc::new(100);
    let _guard = value.root_guard();

    assert_eq!(*value, 100);
}

#[test]
fn test_gc_main_macro_basic() {
    test_main_macro();
    assert_eq!(MAIN_CALLED.load(Ordering::SeqCst), 1);
}

#[test]
fn test_gc_main_macro_current_thread() {
    test_main_macro_current_thread();
}

#[test]
fn test_root_guard_multiple_roots() {
    MAIN_CALLED.store(0, Ordering::SeqCst);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let value1 = Gc::new(1);
        let value2 = Gc::new(2);
        let value3 = Gc::new(3);

        let _guard1 = value1.root_guard();
        let _guard2 = value2.root_guard();
        let _guard3 = value3.root_guard();

        MAIN_CALLED.fetch_add(1, Ordering::SeqCst);
        assert_eq!(*value1, 1);
        assert_eq!(*value2, 2);
        assert_eq!(*value3, 3);
    });

    let rt2 = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt2.block_on(async {
        let value1 = Gc::new(1);
        let _guard = value1.root_guard();

        MAIN_CALLED.fetch_add(1, Ordering::SeqCst);
        assert_eq!(*value1, 1);
    });

    assert_eq!(MAIN_CALLED.load(Ordering::SeqCst), 2);
}

#[test]
fn test_root_guard_nested() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let value = Gc::new(999);
        let _outer_guard = value.root_guard();

        {
            let inner_value = Gc::new(888);
            let _inner_guard = inner_value.root_guard();

            assert_eq!(*value, 999);
            assert_eq!(*inner_value, 888);
        }
    });
}
