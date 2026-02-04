//! Tests for the `GcCell` derive macro.

use rudo_gc::{cell::GcCell, Gc, Trace};
use rudo_gc_derive::GcCell;

#[derive(Trace, GcCell)]
struct BasicStruct {
    gc_field: Gc<i32>,
    regular_field: String,
}

#[derive(Trace, GcCell)]
struct VecStruct {
    gc_vec: Vec<Gc<i32>>,
}

#[derive(Trace, GcCell)]
struct OptionStruct {
    gc_option: Option<Gc<i32>>,
}

#[derive(Trace, GcCell)]
struct NestedStruct {
    inner: InnerStruct,
}

#[derive(Trace)]
struct InnerStruct {
    gc_field: Gc<i32>,
}

#[derive(Trace, GcCell)]
struct NoGcStruct {
    field: i32,
    string: String,
}

#[derive(Trace, GcCell)]
struct ComplexStruct {
    gc1: Gc<i32>,
    gc_vec: Vec<Gc<String>>,
    gc_option: Option<Gc<()>>,
    regular_field: bool,
}

#[derive(Trace, GcCell)]
struct UnnamedStruct(Gc<i32>, String);

#[test]
fn test_basic_struct() {
    let cell = GcCell::new(BasicStruct {
        gc_field: Gc::new(42),
        regular_field: "hello".to_string(),
    });

    // Should be able to borrow_mut
    let mut borrow = cell.borrow_mut();
    assert_eq!(*borrow.gc_field, 42);
    borrow.gc_field = Gc::new(100);
    assert_eq!(*borrow.gc_field, 100);
}

#[test]
fn test_vec_struct() {
    let cell = GcCell::new(VecStruct {
        gc_vec: vec![Gc::new(1), Gc::new(2), Gc::new(3)],
    });

    let mut borrow = cell.borrow_mut();
    borrow.gc_vec.push(Gc::new(4));
    assert_eq!(borrow.gc_vec.len(), 4);
}

#[test]
fn test_option_struct() {
    let cell = GcCell::new(OptionStruct { gc_option: None });

    let mut borrow = cell.borrow_mut();
    borrow.gc_option = Some(Gc::new(42));
    assert_eq!(borrow.gc_option, Some(Gc::new(42)));
}

#[test]
fn test_nested_struct() {
    let cell = GcCell::new(NestedStruct {
        inner: InnerStruct {
            gc_field: Gc::new(99),
        },
    });

    let mut borrow = cell.borrow_mut();
    borrow.inner.gc_field = Gc::new(77);
    assert_eq!(*borrow.inner.gc_field, 77);
}

#[test]
fn test_no_gc_struct() {
    let cell = GcCell::new(NoGcStruct {
        field: 123,
        string: "test".to_string(),
    });

    let mut borrow = cell.borrow_mut();
    borrow.field = 456;
    assert_eq!(borrow.field, 456);
}

#[test]
fn test_complex_struct() {
    let cell = GcCell::new(ComplexStruct {
        gc1: Gc::new(1),
        gc_vec: vec![Gc::new("test".to_string())],
        gc_option: None,
        regular_field: true,
    });

    let mut borrow = cell.borrow_mut();
    borrow.gc1 = Gc::new(100);
    borrow.gc_vec.push(Gc::new("test2".to_string()));
    borrow.gc_option = Some(Gc::new(()));
    borrow.regular_field = false;

    assert_eq!(*borrow.gc1, 100);
    assert_eq!(borrow.gc_vec.len(), 2);
    assert!(borrow.gc_option.is_some());
    assert!(!borrow.regular_field);
}

#[test]
fn test_unnamed_struct() {
    let cell = GcCell::new(UnnamedStruct(Gc::new(55), "unnamed".to_string()));

    let mut borrow = cell.borrow_mut();
    borrow.0 = Gc::new(66);
    assert_eq!(*borrow.0, 66);
}
