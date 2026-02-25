//! Regression test for Bug 82: `BinaryHeap` missing `GcCapture` implementation.
//!
//! `BinaryHeap<T>` from `std::collections` lacked `GcCapture` trait implementation,
//! causing SATB write barriers to fail when the container holds Gc<T> pointers.
//!
//! See: docs/issues/2026-02-23_ISSUE_bug82_binaryheap_missing_gccapture.md

use rudo_gc::{collect_full, Gc, Trace};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Clone, Trace, PartialEq, Eq)]
struct Data {
    value: i32,
}

impl PartialOrd for Data {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Data {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

#[test]
fn test_binaryheap_gccapture() {
    let old = Gc::new(RefCell::new(BinaryHeap::new()));

    collect_full();

    {
        let young = Gc::new(Data { value: 42 });
        old.borrow_mut().push(young);
    }

    rudo_gc::collect();

    {
        let borrow = old.borrow();
        if let Some(gc) = borrow.peek() {
            assert_eq!(gc.value, 42);
        }
    }
}
