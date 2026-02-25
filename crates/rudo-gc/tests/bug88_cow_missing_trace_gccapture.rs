//! Regression test for Bug 88: `std::borrow::Cow` missing `Trace` and `GcCapture`.
//!
//! `Cow<T>` lacked `Trace` and `GcCapture` implementations, preventing use of
//! Clone-on-Write patterns with GC pointers.
//!
//! See: docs/issues/2026-02-23_ISSUE_bug88_cow_missing_trace_gccapture.md

use rudo_gc::{collect_full, Gc, GcCell, Trace};
use std::borrow::Cow;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_cow_trace_borrowed() {
    let gc = Gc::new(Data { value: 42 });
    let cow: Cow<'_, Gc<Data>> = Cow::Borrowed(&gc);
    collect_full();
    assert_eq!(cow.value, 42);
}

#[test]
fn test_cow_trace_owned() {
    let gc = Gc::new(Data { value: 42 });
    let cow: Cow<'_, Gc<Data>> = Cow::Owned(gc);
    collect_full();
    assert_eq!(cow.value, 42);
}

#[test]
fn test_cow_gccapture_in_gccell() {
    let cell = Gc::new(GcCell::new(Cow::<'_, Gc<Data>>::Owned(Gc::new(Data {
        value: 42,
    }))));

    collect_full();

    {
        let borrow = cell.borrow();
        assert_eq!(borrow.value, 42);
    }
}
