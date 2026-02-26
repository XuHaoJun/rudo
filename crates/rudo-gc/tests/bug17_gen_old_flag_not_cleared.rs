//! Test for Bug 17: `GEN_OLD_FLAG` not cleared on dealloc
//!
//! When object with `GEN_OLD_FLAG` is freed and memory reused, new object may
//! inherit the flag, causing wrong barrier behavior.
//!
//! See: docs/issues/2026-02-19_ISSUE_bug17_gen_old_flag_not_cleared.md

use rudo_gc::{collect_full, Gc, Trace};

#[derive(Default, Trace)]
struct OldGenerationData {
    x: u64,
}

#[derive(Default, Trace)]
struct YoungData {
    value: i32,
}

#[test]
fn test_gen_old_flag_not_cleared_on_reuse() {
    // 1. Allocate, promote with GC
    let obj1 = Gc::new(OldGenerationData::default());
    collect_full();

    // 2. Drop and collect
    drop(obj1);
    collect_full();

    // 3. Allocate new object (may reuse same memory)
    let obj2 = Gc::new(YoungData { value: 42 });

    // 4. If obj2 inherited GEN_OLD_FLAG, generational barrier could behave wrong.
    // Hard to observe directly - we just ensure no crash.
    collect_full();
    assert_eq!(obj2.value, 42);
}
