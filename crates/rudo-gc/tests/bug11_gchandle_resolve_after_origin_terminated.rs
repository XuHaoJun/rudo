//! Test for Bug 11: `GcHandle::resolve()` panics when origin thread terminated
//!
//! See: docs/issues/2026-02-19_ISSUE_bug11_gchandle_origin_thread_terminated.md

use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_resolve_after_origin_terminated_panics() {
    let handle = std::thread::spawn(|| {
        let gc = Gc::new(Data { value: 42 });
        gc.cross_thread_handle()
    })
    .join()
    .unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = handle.resolve();
    }));

    assert!(
        result.is_err(),
        "resolve() should panic when called from non-origin thread after origin terminated"
    );
}
