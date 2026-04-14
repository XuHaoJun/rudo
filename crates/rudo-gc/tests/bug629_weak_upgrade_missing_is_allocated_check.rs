//! Regression test for Bug 629: `Weak::upgrade()` missing `is_allocated` check.
//!
//! `Weak::upgrade()` had a TOCTOU race condition where:
//! 1. Slot passed `is_allocated` check at line 2429
//! 2. Slot was swept by lazy sweep before `gc_box` dereference at line 2431
//! 3. Dereferencing swept slot caused UAF
//!
//! The fix adds `is_allocated` check immediately after `gc_box` dereference
//! to prevent UAF when slot is swept between pre-check and dereference.
//!
//! See: docs/issues/2026-04-13_ISSUE_bug629_weak_upgrade_missing_is_allocated_check.md

#![cfg(feature = "test-util")]

use rudo_gc::test_util;
use rudo_gc::{collect_full, Gc, Trace};

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_weak_upgrade_no_uaf_after_sweep() {
    test_util::reset();

    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);

    drop(gc);

    for _ in 0..1000 {
        if let Some(_upgraded) = weak.upgrade() {
            // If we get here without panic/UAF, the fix is working
        }
    }

    collect_full();

    assert!(
        weak.upgrade().is_none(),
        "Weak should be invalid after collection"
    );
}
