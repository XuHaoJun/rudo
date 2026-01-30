#![feature(proc_macro_hygiene)]
#![feature(stmt_expr_attributes)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::let_unit_value)]
#![allow(clippy::ignored_unit_patterns)]

//! Integration tests for tokio proc-macros: #[gc_main] and #[gc::root]

use rudo_gc::tokio::{gc_main, gc_root};
use rudo_gc::Gc;
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

static MAIN_CALLED: AtomicUsize = AtomicUsize::new(0);
static ROOT_CALLED: AtomicUsize = AtomicUsize::new(0);

#[gc_main]
async fn test_main_macro() {
    MAIN_CALLED.fetch_add(1, Ordering::SeqCst);

    let value = Gc::new(42);
    let _ = #[gc_root]
    async {
        ROOT_CALLED.fetch_add(1, Ordering::SeqCst);
        assert_eq!(*value, 42);
    };

    assert_eq!(MAIN_CALLED.load(Ordering::SeqCst), 1);
    assert_eq!(ROOT_CALLED.load(Ordering::SeqCst), 1);
}

#[gc_main(flavor = "current_thread")]
async fn test_main_macro_current_thread() {
    let value = Gc::new(100);
    let _ = #[gc_root]
    async {
        assert_eq!(*value, 100);
    };
}

#[test]
fn test_gc_main_macro_basic() {
    test_main_macro();
    assert_eq!(MAIN_CALLED.load(Ordering::SeqCst), 1);
    assert_eq!(ROOT_CALLED.load(Ordering::SeqCst), 1);
}

#[test]
fn test_gc_main_macro_current_thread() {
    test_main_macro_current_thread();
}

#[test]
fn test_gc_root_macro_multiple_roots() {
    ROOT_CALLED.store(0, Ordering::SeqCst);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let value1 = Gc::new(1);
        let value2 = Gc::new(2);
        let value3 = Gc::new(3);

        let _ = #[gc_root]
        async {
            ROOT_CALLED.fetch_add(1, Ordering::SeqCst);
            assert_eq!(*value1, 1);
            assert_eq!(*value2, 2);
            assert_eq!(*value3, 3);
        };

        let _ = #[gc_root]
        async {
            ROOT_CALLED.fetch_add(1, Ordering::SeqCst);
            assert_eq!(*value1, 1);
        };
    });

    assert_eq!(ROOT_CALLED.load(Ordering::SeqCst), 2);
}

#[test]
fn test_gc_root_macro_nested() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let value = Gc::new(999);

        let _ = #[gc_root]
        async {
            let inner_value = Gc::new(888);

            let _ = #[gc_root]
            async {
                assert_eq!(*value, 999);
                assert_eq!(*inner_value, 888);
            };
        };
    });
}

#[test]
fn test_gc_root_macro_with_refcell() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let cell = RefCell::new(Gc::new(42));

        let _ = #[gc_root]
        async {
            let val: &Gc<i32> = &cell.borrow();
            assert_eq!(**val, 42);
        };
    });
}
