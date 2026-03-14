//! Regression test for Bug 223: `RefCell` `GcCapture` using `try_borrow()` causes silent failure.
//!
//! When `RefCell` is mutably borrowed, `try_borrow()` returns Err and the implementation
//! would silently skip capturing GC pointers, violating the `GcCapture` contract and
//! potentially causing UAF. The fix uses `borrow()` for explicit failure (panic) instead.

#![cfg(feature = "test-util")]

use rudo_gc::cell::GcCapture;
use rudo_gc::test_util;
use rudo_gc::{Gc, Trace};
use std::cell::RefCell;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_refcell_gccapture_captures_when_not_borrowed() {
    test_util::reset();

    // Verify `RefCell` correctly captures GC pointers when not borrowed
    let inner = Gc::new(Data { value: 42 });
    let cell = RefCell::new(inner);

    let mut ptrs = Vec::new();
    cell.capture_gc_ptrs_into(&mut ptrs);

    assert_eq!(ptrs.len(), 1, "Should capture the inner Gc pointer");
}

#[test]
#[should_panic(expected = "already mutably borrowed")]
fn test_refcell_gccapture_panics_when_borrowed() {
    test_util::reset();

    // With the fix: `borrow()` panics when `RefCell` is mutably borrowed.
    // This is correct - explicit failure is preferable to silent UAF.
    let cell = RefCell::new(vec![Gc::new(Data { value: 1 })]);

    let _mut_borrow = cell.borrow_mut();
    // This should panic - we cannot capture while mutably borrowed
    cell.capture_gc_ptrs_into(&mut Vec::new());
}
