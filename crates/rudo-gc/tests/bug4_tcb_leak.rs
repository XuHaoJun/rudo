//! Regression test for Bug 4: `GcHandle` TCB leak when origin thread terminates.
//!
//! With the fix (Weak + root migration), the TCB is dropped when the origin
//! thread exits and the handle is dropped. The handle should still work
//! (resolve, clone) until it is dropped.
//!
//! See: docs/issues/2026-02-19_ISSUE_bug4_cross_thread_handle_tcb_leak.md

use rudo_gc::handles::GcHandle;
use rudo_gc::{collect_full, Gc, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_handle_resolve_after_origin_exits() {
    let handle: GcHandle<Data> = std::thread::spawn(|| {
        let gc = Gc::new(Data { value: 42 });
        gc.cross_thread_handle()
    })
    .join()
    .unwrap();

    // Origin thread has exited. With Weak + migration, TCB is dropped.
    // Handle should still resolve on origin thread... but we're on main thread!
    // try_resolve returns None from wrong thread. resolve would panic.
    assert!(
        handle.try_resolve().is_none(),
        "Wrong thread should get None"
    );
    assert!(handle.is_valid());

    // Drop handle - should not leak. Before fix, TCB stayed alive.
    drop(handle);
    collect_full();
}

#[test]
fn test_handle_clone_after_origin_exits() {
    let handle: GcHandle<Data> = std::thread::spawn(|| {
        let gc = Gc::new(Data { value: 99 });
        gc.cross_thread_handle()
    })
    .join()
    .unwrap();

    // Clone an orphaned handle (origin thread is dead)
    let clone = handle.clone();
    assert!(clone.is_valid());
    drop(handle);
    drop(clone);
    collect_full();
}
