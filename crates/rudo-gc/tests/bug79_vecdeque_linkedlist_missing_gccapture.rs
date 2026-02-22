//! Regression test for Bug 79: `VecDeque` and `LinkedList` missing `GcCapture` implementation.
//!
//! `VecDeque<T>` and `LinkedList<T>` from `std::collections` lack `GcCapture` trait implementations,
//! causing SATB write barriers to fail when these containers hold Gc<T> pointers.
//!
//! See: docs/issues/2026-02-23_ISSUE_bug79_vecdeque_linkedlist_missing_gccapture.md

use rudo_gc::{collect_full, Gc, Trace};
use std::cell::RefCell;
use std::collections::{LinkedList, VecDeque};

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_vecdeque_gccapture_missing() {
    let old = Gc::new(RefCell::new(VecDeque::new()));

    collect_full();

    {
        let young = Gc::new(Data { value: 42 });
        old.borrow_mut().push_back(young);
    };

    rudo_gc::collect();

    {
        let borrow = old.borrow();
        if let Some(gc) = borrow.front() {
            assert_eq!(gc.value, 42);
        }
    }
}

#[test]
fn test_linkedlist_gccapture_missing() {
    let old = Gc::new(RefCell::new(LinkedList::new()));

    collect_full();

    {
        let young = Gc::new(Data { value: 99 });
        old.borrow_mut().push_back(young);
    };

    rudo_gc::collect();

    {
        let borrow = old.borrow();
        if let Some(gc) = borrow.front() {
            assert_eq!(gc.value, 99);
        }
    }
}
